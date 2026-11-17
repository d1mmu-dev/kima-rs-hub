//! Regression coverage for admission at the source gate.

use std::future::Future;
use std::task::Poll;
use std::time::Duration;

use kima_net::gate::Gate;

#[tokio::test(start_paused = true)]
async fn interactive_can_take_cell_before_already_queued_background() {
    let gate = Gate::new(Duration::from_secs(1), 1, 3);
    drop(gate.acquire(None).await.unwrap());
    let priority = kima_net::gate::Priority::background();
    let mut background = Box::pin(gate.acquire_with_priority(None, &priority));
    std::future::poll_fn(|cx| {
        assert!(matches!(background.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;
    let priority = kima_net::gate::Priority::interactive();
    let mut interactive = Box::pin(gate.acquire_with_priority(None, &priority));
    std::future::poll_fn(|cx| {
        assert!(matches!(interactive.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;
    tokio::time::advance(Duration::from_secs(1)).await;
    std::future::poll_fn(|cx| {
        assert!(
            matches!(interactive.as_mut().poll(cx), Poll::Ready(Ok(_))),
            "background reserved the cell before interactive admission"
        );
        Poll::Ready(())
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn cancelled_queued_background_does_not_reserve_the_next_cell() {
    let gate = Gate::new(Duration::from_secs(1), 1, 2);
    drop(gate.acquire(None).await.unwrap());
    let mut background = Box::pin(gate.acquire(None));
    std::future::poll_fn(|cx| {
        assert!(matches!(background.as_mut().poll(cx), Poll::Pending));
        Poll::Ready(())
    })
    .await;
    drop(background);
    tokio::time::advance(Duration::from_secs(1)).await;
    assert!(
        gate.acquire(Some(Duration::ZERO)).await.is_ok(),
        "cancelled background reserved the next cell"
    );
}

#[tokio::test(start_paused = true)]
async fn registered_interactive_precedes_registered_background() {
    use kima_net::gate::Priority;
    let gate = Gate::new(Duration::from_secs(1), 1, 1);
    let held = gate.acquire(None).await.unwrap();
    let background_priority = Priority::background();
    let mut background = Box::pin(gate.acquire_with_priority(None, &background_priority));
    let interactive_priority = Priority::interactive();
    let mut interactive = Box::pin(gate.acquire_with_priority(None, &interactive_priority));
    std::future::poll_fn(|cx| {
        assert!(background.as_mut().poll(cx).is_pending());
        assert!(interactive.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(gate.queued(), 2);
    drop(held);
    tokio::time::advance(Duration::from_secs(1)).await;
    std::future::poll_fn(|cx| {
        assert!(
            background.as_mut().poll(cx).is_pending(),
            "background overtook queued interactive"
        );
        assert!(
            interactive.as_mut().poll(cx).is_ready(),
            "interactive was not admitted"
        );
        Poll::Ready(())
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn raising_registered_waiter_and_cancelling_winner() {
    use kima_net::gate::Priority;
    let gate = Gate::new(Duration::from_secs(1), 1, 1);
    let held = gate.acquire(None).await.unwrap();
    let first_priority = Priority::background();
    let raised_priority = Priority::background();
    let mut first = Box::pin(gate.acquire_with_priority(None, &first_priority));
    let mut raised = Box::pin(gate.acquire_with_priority(None, &raised_priority));
    std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        assert!(raised.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(gate.queued(), 2);
    raised_priority.raise();
    drop(held);
    tokio::time::advance(Duration::from_secs(1)).await;
    std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(raised);
    assert_eq!(gate.queued(), 1);
    std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_ready());
        Poll::Ready(())
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn equal_priority_is_fifo_even_when_second_is_polled_first() {
    use kima_net::gate::Priority;
    let gate = Gate::new(Duration::from_secs(1), 1, 1);
    let held = gate.acquire(None).await.unwrap();
    let p1 = Priority::interactive();
    let p2 = Priority::interactive();
    let mut first = Box::pin(gate.acquire_with_priority(None, &p1));
    let mut second = Box::pin(gate.acquire_with_priority(None, &p2));
    std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        assert!(second.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(gate.queued(), 2);
    drop(held);
    tokio::time::advance(Duration::from_secs(1)).await;
    std::future::poll_fn(|cx| {
        assert!(second.as_mut().poll(cx).is_pending());
        assert!(first.as_mut().poll(cx).is_ready());
        Poll::Ready(())
    })
    .await;
}

#[tokio::test(start_paused = true)]
async fn raised_registered_background_is_admitted_before_older_background() {
    use kima_net::gate::Priority;
    let gate = Gate::new(Duration::from_secs(1), 1, 1);
    let held = gate.acquire(None).await.unwrap();
    let p1 = Priority::background();
    let p2 = Priority::background();
    let mut first = Box::pin(gate.acquire_with_priority(None, &p1));
    let mut second = Box::pin(gate.acquire_with_priority(None, &p2));
    std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        assert!(second.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    assert_eq!(gate.queued(), 2);
    p2.raise();
    drop(held);
    tokio::time::advance(Duration::from_secs(1)).await;
    std::future::poll_fn(|cx| {
        assert!(first.as_mut().poll(cx).is_pending());
        assert!(second.as_mut().poll(cx).is_ready());
        Poll::Ready(())
    })
    .await;
}
