#!/usr/bin/env python3
"""Loopback stand-ins for tools/capture-api-fixtures.sh's podcast captures (M6 Task 19).

One HTTP server on 127.0.0.1 (OS-assigned port, printed on stdout before serving) answers:

* Apple's iTunes API as kima-podcast's catalogue asks it (crates/kima-podcast/src/catalog.rs):
  `/WebObjects/MZStoreServices.woa/ws/genres`, `/<cc>/rss/toppodcasts/limit=N[/genre=G]/json`,
  `/lookup?id=a,b`, `/search?term=...`. Synthetic data only; no real Apple recording.
* Three RSS feeds (`/feeds/race.xml`, `/feeds/robot.xml`, `/feeds/preview.xml`), their audio
  (`/audio/<name>.mp3`, range-aware) and one PNG cover (`/img/<name>.png`).
* `POST /control/race-new-episode`: from then on `/feeds/race.xml` also lists "Pit Stop", so a
  manual refresh finds one new episode.

Shows (Apple id -> feed, categories, AI tag per Podcasting 2.0 `podcast:txt purpose="ai-content"`):
  555 Race Show     /feeds/race.xml     [1495, 1303]  no channel tag (unknown); item tags:
                    "Sprint" false (human), "Race Recap" true (ai), others none
  777 Robot Radio   /feeds/robot.xml    [1318]        channel tag true (ai)
  888 Preview Show  /feeds/preview.xml  [1495, 1303]  no tag; never followed (preview/snippet)
  999 Chart Only    no feed             [1318]        appears in charts only
The server is a capture aid: it keeps no state beyond the control switch and exits when killed.
"""
import http.server
import json
import re
import socketserver
import struct
import sys
import threading
import urllib.parse
import zlib

BASE = ""  # set in main(): http://127.0.0.1:<port>
NEW_EPISODE = threading.Event()

GENRES = {
    "26": {
        "name": "Podcasts",
        "id": "26",
        "subgenres": {
            "1303": {
                "name": "Comedy",
                "id": "1303",
                "subgenres": {"1495": {"name": "Improv", "id": "1495"}},
            },
            "1318": {"name": "Technology", "id": "1318"},
        },
    }
}

SHOWS = [
    # (itunes id, title, author, summary, feed path or None, genre ids, track count)
    ("555", "Race Show", "Race Author", "A show about racing.", "/feeds/race.xml", [1495, 1303], 4),
    ("777", "Robot Radio", "Robot Author", "Radio made by a machine.", "/feeds/robot.xml", [1318], 2),
    ("888", "Preview Show", "Preview Author", "A show to preview before following.", "/feeds/preview.xml", [1495, 1303], 2),
    ("999", "Chart Only", "Chart Author", "Only on the chart.", None, [1318], 7),
]

CHART_PATH = re.compile(r"^/[a-z]{2}/rss/toppodcasts/limit=\d+(?:/genre=(\d+))?/json$")


def cover(name):
    return f"{BASE}/img/{name}.png"


def chart_entry(show):
    sid, title, author, summary, _feed, genres, _count = show
    return {
        "id": {"attributes": {"im:id": sid}},
        "im:name": {"label": title},
        "im:artist": {"label": author},
        "summary": {"label": summary},
        "im:image": [{"label": cover(f"apple-{sid}")}],
        "category": {"attributes": {"im:id": str(genres[0])}},
    }


def apple_record(show, search=False):
    sid, title, author, summary, feed, genres, count = show
    record = {
        "kind": "podcast",
        "collectionId": int(sid),
        "collectionName": title,
        "artistName": author,
        "artworkUrl600": cover(f"apple-{sid}"),
        "genreIds": [str(g) for g in genres] + ["26"],
        "genres": [GENRE_NAMES[g] for g in genres] + ["Podcasts"],
        "trackCount": count,
    }
    if feed:
        record["feedUrl"] = BASE + feed
    if search:
        record["description"] = summary
    return record


GENRE_NAMES = {1303: "Comedy", 1495: "Improv", 1318: "Technology"}

NS = (
    'xmlns:itunes="http://www.itunes.com/dtds/podcast-1.0.dtd" '
    'xmlns:podcast="https://podcastindex.org/namespace/1.0"'
)


def item(guid, title, date, seconds, description=None, ai=None, number=None, season=None, image=None):
    parts = [f"<item><title>{title}</title><guid>{guid}</guid><pubDate>{date}</pubDate>"]
    if description is not None:
        parts.append(f"<description>{description}</description>")
    parts.append(f"<itunes:duration>{seconds}</itunes:duration>")
    if number is not None:
        parts.append(f"<itunes:episode>{number}</itunes:episode>")
    if season is not None:
        parts.append(f"<itunes:season>{season}</itunes:season>")
    if image is not None:
        parts.append(f'<itunes:image href="{cover(image)}"/>')
    if ai is not None:
        parts.append(f'<podcast:txt purpose="ai-content">{"true" if ai else "false"}</podcast:txt>')
    parts.append(f'<enclosure url="{BASE}/audio/{guid}.mp3" length="{len(AUDIO)}" type="audio/mpeg"/></item>')
    return "".join(parts)


def channel(title, author, description, image, items, ai=None, categories=()):
    tag = "" if ai is None else f'<podcast:txt purpose="ai-content">{"true" if ai else "false"}</podcast:txt>'
    cats = "".join(f'<itunes:category text="{c}"/>' for c in categories)
    return (
        f'<?xml version="1.0" encoding="UTF-8"?><rss version="2.0" {NS}><channel>'
        f"<title>{title}</title><itunes:author>{author}</itunes:author>"
        f"<description>{description}</description><language>en</language>"
        f'<image><url>{cover(image)}</url></image>{cats}{tag}{"".join(items)}</channel></rss>'
    )


def race_feed():
    items = [
        item("race-gp", "Grand Prix", "Sun, 08 Nov 2026 10:00:00 GMT", 3600,
             "The main race, start to flag.", number=4, season=1, image="race-gp"),
        item("race-recap", "Race Recap", "Sat, 07 Nov 2026 10:00:00 GMT", 900,
             "A generated race recap.", ai=True, number=3, season=1),
        item("race-sprint", "Sprint", "Fri, 06 Nov 2026 10:00:00 GMT", 1200,
             "The sprint race.", ai=False, number=2, season=1),
        item("race-quali", "Qualifying", "Thu, 05 Nov 2026 10:00:00 GMT", 1800,
             None, number=1, season=1),
    ]
    if NEW_EPISODE.is_set():
        items.insert(0, item("race-pit", "Pit Stop", "Mon, 09 Nov 2026 10:00:00 GMT", 300,
                             "A short race bulletin."))
    return channel("Race Show", "Race Author", "Everything about the race weekend.", "race",
                   items, categories=("Improv",))


def robot_feed():
    items = [
        item("robot-2", "Robot Hour 2", "Sat, 07 Nov 2026 12:00:00 GMT", 1500, "Second race hour."),
        item("robot-1", "Robot Hour 1", "Fri, 06 Nov 2026 12:00:00 GMT", 1500, "First race hour."),
    ]
    return channel("Robot Radio", "Robot Author", "Synthetic race radio.", "robot", items, ai=True,
                   categories=("Technology",))


def preview_feed():
    items = [
        item("preview-2", "Preview Two", "Sat, 07 Nov 2026 08:00:00 GMT", 640, "The newer one."),
        item("preview-1", "Preview One", "Wed, 04 Nov 2026 08:00:00 GMT", 420, "The first one."),
    ]
    return channel("Preview Show", "Preview Author", "A show to preview.", "preview", items)


FEEDS = {"/feeds/race.xml": race_feed, "/feeds/robot.xml": robot_feed, "/feeds/preview.xml": preview_feed}

# Bytes standing in for an MP3: the capture checks status codes, never decodes audio.
AUDIO = b"ID3\x03\x00\x00\x00\x00\x00\x00" + bytes(range(256)) * 4


def png():
    def chunk(kind, data):
        body = kind + data
        return struct.pack(">I", len(data)) + body + struct.pack(">I", zlib.crc32(body) & 0xFFFFFFFF)

    raw = b"".join(b"\x00" + b"\x80\x40\x20" * 8 for _ in range(8))
    return (b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", 8, 8, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b""))


PNG = png()


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def _send(self, status, body, content_type, extra=None):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        for k, v in (extra or {}).items():
            self.send_header(k, v)
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def _json(self, body):
        self._send(200, json.dumps(body).encode(), "application/json")

    def do_HEAD(self):  # noqa: N802
        self.do_GET()

    def do_POST(self):  # noqa: N802
        if self.path == "/control/race-new-episode":
            NEW_EPISODE.set()
            self._send(204, b"", "text/plain")
        else:
            self._send(404, b"", "text/plain")

    def do_GET(self):  # noqa: N802
        url = urllib.parse.urlsplit(self.path)
        path, query = url.path, urllib.parse.parse_qs(url.query)
        chart = CHART_PATH.match(path)
        if path == "/WebObjects/MZStoreServices.woa/ws/genres":
            self._json(GENRES)
        elif chart:
            genre = chart.group(1)
            shows = [s for s in SHOWS if genre is None or int(genre) in s[5]]
            self._json({"feed": {"entry": [chart_entry(s) for s in shows]}})
        elif path == "/lookup":
            ids = ",".join(query.get("id", [""])).split(",")
            found = [apple_record(s) for s in SHOWS if s[0] in ids]
            self._json({"resultCount": len(found), "results": found})
        elif path == "/search":
            term = query.get("term", [""])[0].lower()
            found = [apple_record(s, search=True) for s in SHOWS if term and term in s[1].lower()]
            self._json({"resultCount": len(found), "results": found})
        elif path in FEEDS:
            self._send(200, FEEDS[path]().encode(), "application/rss+xml")
        elif path.startswith("/audio/") and path.endswith(".mp3"):
            self._audio()
        elif path.startswith("/img/") and path.endswith(".png"):
            self._send(200, PNG, "image/png")
        else:
            self._send(404, b"", "text/plain")

    def _audio(self):
        size = len(AUDIO)
        m = re.fullmatch(r"bytes=(\d*)-(\d*)", self.headers.get("Range", "") or "")
        if not m or (not m.group(1) and not m.group(2)):
            self._send(200, AUDIO, "audio/mpeg", {"Accept-Ranges": "bytes"})
            return
        if m.group(1):
            start = int(m.group(1))
            end = min(int(m.group(2)) if m.group(2) else size - 1, size - 1)
        else:
            start, end = max(size - int(m.group(2)), 0), size - 1
        if start >= size or start > end:
            self._send(416, b"", "text/plain", {"Content-Range": f"bytes */{size}"})
            return
        self._send(206, AUDIO[start:end + 1], "audio/mpeg",
                   {"Accept-Ranges": "bytes", "Content-Range": f"bytes {start}-{end}/{size}"})

    def log_message(self, format, *args):  # noqa: A002
        sys.stderr.write("mock %s\n" % (format % args))


class Server(socketserver.ThreadingMixIn, http.server.HTTPServer):
    daemon_threads = True


def main():
    global BASE
    server = Server(("127.0.0.1", 0), Handler)
    BASE = f"http://127.0.0.1:{server.server_address[1]}"
    print(server.server_address[1], flush=True)
    server.serve_forever()


if __name__ == "__main__":
    sys.exit(main())
