/**
 * What the client reports as heard. The server judges plays from this number,
 * so dragging the scrubber to the end must add almost nothing.
 */

import { describe, expect, it } from "vitest";
import { ListenMeter, requiredSecondsFor } from "../usePlayReporting";

/** Play forwards in realistic 0.25s ticks; returns how many times it reported. */
function play(meter: ListenMeter, from: number, to: number, step = 0.25): number {
    let reports = 0;
    for (let t = from; t <= to + 1e-9; t += step) if (meter.observe(t)) reports++;
    return reports;
}

describe("requiredSecondsFor", () => {
    it("mirrors the server: half the length, capped at four minutes", () => {
        expect(requiredSecondsFor(200)).toBe(100);
        expect(requiredSecondsFor(600)).toBe(240);
    });

    it("refuses a track at or under thirty seconds", () => {
        expect(requiredSecondsFor(30)).toBeNull();
        expect(requiredSecondsFor(10)).toBeNull();
    });

    it("treats an unknown length as the four-minute bound, not a free pass", () => {
        expect(requiredSecondsFor(0)).toBe(240);
        expect(requiredSecondsFor(undefined)).toBe(240);
        expect(requiredSecondsFor(null)).toBe(240);
    });
});

describe("ListenMeter", () => {
    it("reports once real listening passes the threshold", () => {
        const m = new ListenMeter(200);
        expect(play(m, 0, 101)).toBe(1);
    });

    it("reports once, not on every tick after the threshold", () => {
        const m = new ListenMeter(200);
        expect(play(m, 0, 190)).toBe(1);
    });

    it("stays quiet for a track that was barely played", () => {
        const m = new ListenMeter(200);
        expect(play(m, 0, 20)).toBe(0);
    });

    it("gives a scrub to the end almost nothing", () => {
        // The whole point. One jump from 0 to 199 must not read as a listen.
        const m = new ListenMeter(200);
        m.observe(0);
        expect(m.observe(199)).toBe(false);
        expect(m.secondsListened).toBeLessThanOrEqual(1.5);
    });

    it("keeps repeated scrubbing cheap", () => {
        const m = new ListenMeter(200);
        for (let i = 0; i < 50; i++) {
            m.observe(0);
            m.observe(199);
        }
        // 50 jumps, at most 1.5s credited each: 75s, short of the 100s required.
        expect(m.secondsListened).toBeLessThan(100);
    });

    it("does not credit a backwards seek", () => {
        const m = new ListenMeter(200);
        play(m, 100, 130);
        const before = m.secondsListened;
        m.observe(10);
        expect(m.secondsListened).toBe(before);
    });

    it("counts a track heard in two sittings around a seek", () => {
        const m = new ListenMeter(200);
        play(m, 0, 60);
        m.breakChain();
        expect(play(m, 120, 166)).toBe(1); // 60 + 46 = 106s of real audio
    });

    it("carries nothing from one track into the next", () => {
        const m = new ListenMeter(200);
        play(m, 0, 90); // not enough on its own
        m.reset(200);
        expect(m.secondsListened).toBe(0);
        expect(play(m, 0, 30)).toBe(0);
    });

    it("never counts a track under the floor, however long it runs", () => {
        const m = new ListenMeter(25);
        expect(play(m, 0, 25)).toBe(0);
        expect(play(m, 0, 25)).toBe(0);
    });

    it("demands four minutes when the duration is unknown", () => {
        const m = new ListenMeter(0);
        expect(play(m, 0, 200)).toBe(0);
        expect(play(m, 200, 245)).toBe(1);
    });

    it("ignores a position that is not a real number", () => {
        const m = new ListenMeter(200);
        m.observe(10);
        expect(m.observe(Number.NaN)).toBe(false);
        expect(m.secondsListened).toBe(0);
    });
});

describe("the same track played twice in a row", () => {
    it("reports the second listen once the meter is reset at the track's end", () => {
        const meter = new ListenMeter(200);
        expect(play(meter, 0, 200)).toBe(1);
        meter.reset(); // what the hook does on "ended" and "gapless-advance"
        expect(play(meter, 0, 200)).toBe(1);
    });
});
