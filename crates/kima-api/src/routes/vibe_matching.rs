//! HTTP adapters for the existing matching and song-path services.
use super::vibe::{failure, parse_limit};
use crate::{auth::AuthUser, error::ApiError, state::AppState};
use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use kima_api_types::vibe::{
    AlchemyResponse, MatchTrack, PathMetadata, PathResponse, PathTrack, SearchAlbum, SearchArtist,
    SimilarResponse,
};
use kima_db::vibe::{
    path::{self, PathError, PathOptions},
    rank::{self, LibraryTrack, RankedTrack, WeightedInput},
    read,
};
use kima_ident::stages::similarity;
use kima_vibe::{formula::SELF_SCORE, song_path::Mode};
use rand::Rng;
use serde_json::{Value, json};
use std::collections::HashMap;
use uuid::Uuid;

fn bad(message: &str) -> ApiError {
    ApiError::BadRequest(message.into())
}

async fn probe(state: &AppState) -> Option<std::sync::Arc<kima_vibe::feel_probe::FeelProbe>> {
    similarity::runtime(&state.ident_ctx())
        .await
        .map(|r| r.probe.clone())
}

async fn match_tracks(
    state: &AppState,
    rows: Vec<RankedTrack>,
    scale: f64,
    error: &'static str,
) -> Result<Vec<MatchTrack>, ApiError> {
    let titles = read::edited_titles(
        &state.pool,
        &rows.iter().map(|r| r.track.id).collect::<Vec<_>>(),
    )
    .await
    .map_err(|e| failure(error, e))?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let t = r.track;
            let m = r.score / scale;
            MatchTrack {
                id: t.id.to_string(),
                title: t.title,
                display_title: titles.get(&t.id).cloned().flatten(),
                duration: t.duration_secs,
                distance: 1.0 - m,
                similarity: m.clamp(0.0, 1.0),
                album: SearchAlbum {
                    id: t.album_id.to_string(),
                    title: t.album_title,
                    cover_url: t.album_has_cover.then(|| t.album_id.to_string()),
                },
                artist: SearchArtist {
                    id: t.artist_id.to_string(),
                    name: t.artist_name,
                },
            }
        })
        .collect())
}

pub async fn similar(
    State(state): State<AppState>,
    _user: AuthUser,
    Path(source): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Response, ApiError> {
    const ERROR: &str = "Failed to find similar tracks";
    let limit = parse_limit(
        params
            .get("limit")
            .map(|s| Value::String(s.clone()))
            .as_ref(),
    );
    let mut rows = Vec::new();
    if let (Ok(id), Some(probe)) = (Uuid::parse_str(&source), probe(&state).await) {
        if let Some(ctx) = rank::matching_context(&state.pool, &probe, &[id])
            .await
            .map_err(|e| failure(ERROR, e))?
        {
            if ctx.tracks.contains_key(&id) {
                rows = rank::rank_matches(
                    &state.pool,
                    &ctx,
                    &[WeightedInput {
                        track_id: id,
                        weight: 1.0,
                    }],
                    limit as i64,
                    &[id],
                )
                .await
                .map_err(|e| failure(ERROR, e))?;
            }
        }
    }
    if rows.is_empty() {
        return Ok((StatusCode::NOT_FOUND,Json(json!({"error":"No similar tracks found","message":"This track may not have been analyzed yet, or no analyzer is running"}))).into_response());
    }
    Ok(Json(SimilarResponse {
        source_track_id: source,
        tracks: match_tracks(&state, rows, SELF_SCORE, ERROR).await?,
    })
    .into_response())
}

// JS truthiness matters for the optional subtract field, before array validation.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn ids(values: &[Value], message: &str) -> Result<Vec<Uuid>, ApiError> {
    values
        .iter()
        .map(|v| {
            v.as_str()
                .and_then(|s| Uuid::parse_str(s).ok())
                .ok_or_else(|| bad(message))
        })
        .collect()
}

pub async fn alchemy(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    const ERROR: &str = "Failed to compute alchemy blend";
    let add = body
        .get("add")
        .and_then(Value::as_array)
        .filter(|v| !v.is_empty())
        .ok_or_else(|| bad("At least one track ID in 'add' is required"))?;
    if add.len() > 10 {
        return Err(bad("'add' may contain at most 10 tracks"));
    }
    let subtract = body.get("subtract").filter(|v| truthy(v));
    if subtract.is_some_and(|v| !v.is_array() || v.as_array().is_some_and(|a| a.len() > 10)) {
        return Err(bad("'subtract' must be an array of at most 10 tracks"));
    }
    let add = ids(add, "'add' must contain valid track IDs")?;
    let subtract = ids(
        subtract
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or_default(),
        "'subtract' must contain valid track IDs",
    )?;
    let all: Vec<_> = add.iter().chain(&subtract).copied().collect();
    let ctx = if let Some(p) = probe(&state).await {
        rank::matching_context(&state.pool, &p, &all)
            .await
            .map_err(|e| failure(ERROR, e))?
    } else {
        None
    };
    let missing_add = ctx
        .as_ref()
        .map_or_else(|| add.clone(), |c| c.missing(&add));
    let missing_subtract = ctx
        .as_ref()
        .map_or_else(|| subtract.clone(), |c| c.missing(&subtract));
    let Some(ctx) = ctx.filter(|_| missing_add.is_empty() && missing_subtract.is_empty()) else {
        return Ok((StatusCode::BAD_REQUEST,Json(json!({"error":"Some tracks have not been through the similarity step yet","missingAdd":missing_add,"missingSubtract":missing_subtract}))).into_response());
    };
    let inputs: Vec<_> = add
        .iter()
        .map(|id| WeightedInput {
            track_id: *id,
            weight: 1.0 / add.len() as f64,
        })
        .chain(subtract.iter().map(|id| WeightedInput {
            track_id: *id,
            weight: -1.0 / subtract.len() as f64,
        }))
        .collect();
    let blend = rank::check_blend(&ctx, &inputs).map_err(|e| match e {
        rank::RankError::CancelsOut => {
            bad("The tracks to add and to subtract cancel each other out")
        }
        other => failure(ERROR, other),
    })?;
    let rows = rank::rank_matches(
        &state.pool,
        &ctx,
        &inputs,
        parse_limit(body.get("limit")) as i64,
        &add,
    )
    .await
    .map_err(|e| failure(ERROR, e))?;
    Ok(Json(AlchemyResponse {
        tracks: match_tracks(&state, rows, (blend * SELF_SCORE).sqrt(), ERROR).await?,
    })
    .into_response())
}

/// StringNumericLiteral, not parseFloat: signed radix prefixes and Rust's `inf` are invalid.
/// Callers must apply JavaScript truthiness *before* coercion (empty string vs whitespace).
fn string_number(s: &str) -> f64 {
    let s = s.trim_matches(kima_vibe::ecmascript_whitespace);
    if s.is_empty() {
        return 0.0;
    }
    let radix = match s.as_bytes().get(..2) {
        Some(b"0x" | b"0X") => Some(16),
        Some(b"0o" | b"0O") => Some(8),
        Some(b"0b" | b"0B") => Some(2),
        _ => None,
    };
    if let Some(radix) = radix {
        let digits: Option<Vec<u32>> = s[2..].chars().map(|c| c.to_digit(radix)).collect();
        let Some(digits) = digits.filter(|d| !d.is_empty()) else {
            return f64::NAN;
        };
        // Power-of-two radix integers round once to binary64, including ties-to-even.
        // Retain the leading 53 bits plus guard/sticky instead of repeatedly rounding.
        let width = radix.trailing_zeros();
        let bits = digits
            .iter()
            .flat_map(|d| (0..width).rev().map(move |b| (d >> b) & 1));
        let mut significant = bits.skip_while(|b| *b == 0);
        let mut mantissa = 0_u64;
        let mut count = 0_usize;
        let mut guard = false;
        let mut sticky = false;
        for bit in significant.by_ref() {
            if count < 53 {
                mantissa = (mantissa << 1) | u64::from(bit);
            } else if count == 53 {
                guard = bit != 0;
            } else {
                sticky |= bit != 0;
            }
            count += 1;
        }
        if guard && (sticky || mantissa & 1 != 0) {
            mantissa += 1;
        }
        if count > 1024 {
            return f64::INFINITY;
        }
        return mantissa as f64 * 2_f64.powi(count.saturating_sub(53) as i32);
    }
    if matches!(s, "Infinity" | "+Infinity") {
        return f64::INFINITY;
    }
    if s == "-Infinity" {
        return f64::NEG_INFINITY;
    }
    // Rust accepts `inf`, `infinity` and case variants that ECMAScript does not.
    if !s
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'))
    {
        return f64::NAN;
    }
    s.parse().unwrap_or(f64::NAN)
}

fn path_length(v: Option<&Value>) -> Option<usize> {
    let v = v.filter(|v| truthy(v))?;
    let n = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => Some(string_number(s)),
        Value::Bool(b) => Some(u8::from(*b) as f64),
        _ => None,
    }?;
    if n.is_nan() {
        None
    } else {
        Some(n.clamp(5.0, 50.0) as usize)
    }
}
fn path_track(t: LibraryTrack, titles: &HashMap<Uuid, Option<String>>) -> PathTrack {
    PathTrack {
        id: t.id.to_string(),
        title: t.title,
        display_title: titles.get(&t.id).cloned().flatten(),
        duration: t.duration_secs,
        album_id: t.album_id.to_string(),
        album_title: t.album_title,
        album_cover_url: t.album_has_cover.then(|| t.album_id.to_string()),
        artist_id: t.artist_id.to_string(),
        artist_name: t.artist_name,
    }
}

pub async fn song_path(
    State(state): State<AppState>,
    _user: AuthUser,
    Json(body): Json<Value>,
) -> Result<Response, ApiError> {
    const ERROR: &str = "Failed to generate song path";
    let pair = body
        .get("startTrackId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .zip(
            body.get("endTrackId")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty()),
        )
        .ok_or_else(|| bad("startTrackId and endTrackId are required strings"))?;
    let start = Uuid::parse_str(pair.0).map_err(|_| bad("Invalid track ID format"))?;
    let end = Uuid::parse_str(pair.1).map_err(|_| bad("Invalid track ID format"))?;
    if start == end {
        return Err(bad("Start and end tracks must be different"));
    }
    let probe = probe(&state)
        .await
        .ok_or_else(|| bad(&PathError::NotMatched.to_string()))?;
    let mode = if body.get("mode").and_then(Value::as_str) == Some("discovery") {
        Mode::Discovery
    } else {
        Mode::Smooth
    };
    let result = path::generate_song_path(
        &state.pool,
        &probe,
        start,
        end,
        PathOptions {
            length: path_length(body.get("length")),
            mode,
        },
        |bound| rand::thread_rng().gen_range(0..bound),
    )
    .await
    .map_err(|e| {
        if e.status() == 400 {
            bad(&e.to_string())
        } else {
            failure(ERROR, e)
        }
    })?;
    let ids: Vec<_> = std::iter::once(result.start.id)
        .chain(result.path.iter().map(|t| t.id))
        .chain(std::iter::once(result.end.id))
        .collect();
    let titles = read::edited_titles(&state.pool, &ids)
        .await
        .map_err(|e| failure(ERROR, e))?;
    Ok(Json(PathResponse {
        start_track: path_track(result.start, &titles),
        end_track: path_track(result.end, &titles),
        path: result
            .path
            .into_iter()
            .map(|t| path_track(t, &titles))
            .collect(),
        metadata: PathMetadata {
            total_tracks: result.total_tracks,
            embedding_distance: result.embedding_distance,
            average_step_size: result.average_step_size,
            mode: if mode == Mode::Discovery {
                "discovery"
            } else {
                "smooth"
            }
            .into(),
        },
    })
    .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ecmascript_string_number_boundaries() {
        for (s, expected) in [
            ("0x10", 16.0),
            ("0b1000", 8.0),
            ("0o12", 10.0),
            ("\u{feff}+1.2e1\u{2029}", 12.0),
            ("", 0.0),
            ("Infinity", f64::INFINITY),
            ("+Infinity", f64::INFINITY),
            ("-Infinity", f64::NEG_INFINITY),
            ("0x20000000000001", 9007199254740992.0),
            ("0x20000000000003", 9007199254740996.0),
        ] {
            assert_eq!(string_number(s), expected, "{s:?}");
        }
        assert!(string_number("-0").is_sign_negative());
        assert_eq!(
            string_number(&format!("0x1{}", "0".repeat(256))),
            f64::INFINITY
        );
        for s in [
            "NaN", "inf", "infinity", "INFINITY", "+0x10", "-0b10", "0o8", "0x", "1_0", "1junk",
            "\u{85}8",
        ] {
            assert!(string_number(s).is_nan(), "{s:?}");
        }
        assert_eq!(path_length(Some(&json!(""))), None);
        assert_eq!(path_length(Some(&json!(" "))), Some(5));
        assert_eq!(path_length(Some(&json!(0))), None);
        assert_eq!(path_length(Some(&json!("-0"))), Some(5));
    }
}
