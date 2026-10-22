//! The Cover Art Archive's front cover of a release group (spec §12.7 step 2; upstream
//! `coverArt.ts`). The URL is returned as text and never fetched here.

use kima_net::{Host, Net, Outcome, Req};
use serde_json::Value;

use crate::Mbid;
use crate::answer::{self, Got, items};

/// The most images looked at.
const MAX_IMAGES: usize = 200;

/// `GET /release-group/{rg}`: the first image marked `front: true`, its `thumbnails.1200`, else
/// its `image`. A 404 or no front image is `None`.
pub async fn front(net: &Net, rg: Mbid) -> Outcome<Option<String>> {
    let req = Req::get(Host::CoverArtArchive, &format!("/release-group/{rg}")).absent_on_404();
    let body = match answer::got(net.get_json::<Value>(req).await) {
        Ok(Got::Answered(body)) => body,
        Ok(Got::Absent(_)) => return Outcome::Answered(None),
        Err(o) => return o,
    };
    let front = items(body.get("images"))
        .iter()
        .take(MAX_IMAGES)
        .find(|i| i.get("front").and_then(Value::as_bool) == Some(true));
    Outcome::Answered(front.and_then(|i| {
        answer::url(i.pointer("/thumbnails/1200")).or_else(|| answer::url(i.get("image")))
    }))
}
