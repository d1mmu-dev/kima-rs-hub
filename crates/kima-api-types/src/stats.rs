use serde::Serializer;
use time::OffsetDateTime;

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TrackPlays {
    pub plays: i64,
}

pub fn js_iso(t: OffsetDateTime) -> String {
    let t = t.to_offset(time::UtcOffset::UTC);
    let year = match t.year() {
        y if y < 0 => format!("-{:06}", y.unsigned_abs()),
        y if y > 9999 => format!("+{y:06}"),
        y => format!("{y:04}"),
    };
    format!(
        "{}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        year,
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}
pub fn js_f64<S: Serializer>(v: &f64, s: S) -> Result<S::Ok, S::Error> {
    assert!(v.is_finite());
    if v.fract() == 0.0 && *v >= i64::MIN as f64 && *v < -(i64::MIN as f64) {
        s.serialize_i64(*v as i64)
    } else {
        s.serialize_f64(*v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Serialize;
    #[test]
    fn js_iso_has_millis_and_z() {
        let t = OffsetDateTime::from_unix_timestamp(1_772_407_800)
            .unwrap()
            .replace_microsecond(123_456)
            .unwrap();
        assert_eq!(js_iso(t), "2026-03-01T23:30:00.123Z");
        assert_eq!(
            js_iso(t.replace_year(-1).unwrap()),
            "-000001-03-01T23:30:00.123Z"
        );
        assert_eq!(
            js_iso(t.replace_nanosecond(0).unwrap()),
            "2026-03-01T23:30:00.000Z"
        );
    }
    #[test]
    fn js_f64_writes_integral_as_integer() {
        #[derive(Serialize)]
        struct Value(#[serde(serialize_with = "js_f64")] f64);
        assert_eq!(serde_json::to_string(&Value(240.0)).unwrap(), "240");
        assert_eq!(serde_json::to_string(&Value(15.5)).unwrap(), "15.5");
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct HourTrack {
    pub title: String,
    pub artist: String,
    pub listens: i64,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct NamedListens {
    pub name: String,
    pub listens: i64,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HourDetail {
    pub weekday: i32,
    pub hour: i32,
    pub listens: i64,
    pub tracks: Vec<HourTrack>,
    pub artists: Vec<NamedListens>,
    pub last_at: Option<String>,
}

#[cfg(test)]
mod listening_tests {
    use super::*;
    #[test]
    fn listening_wire_shape_keeps_empty_analysis_and_sources_object() {
        let value = ListeningStats {
            plays: 1,
            estimated_seconds: 180,
            distinct_tracks: 1,
            distinct_artists: 1,
            distinct_albums: 1,
            first_play: None,
            last_play: None,
            by_source: [("LIBRARY".to_string(), 1)].into_iter().collect(),
            clock: [[0; 24]; 7],
            peak: None,
            shape: vec![],
            drift: vec![DriftMonth {
                month: "2026-04".into(),
                energy: None,
                valence: None,
                bpm: None,
                plays: 1,
            }],
            top_artists: vec![],
            top_tracks: vec![],
        };
        let json = serde_json::to_value(value).unwrap();
        assert_eq!(json["bySource"]["LIBRARY"], 1);
        assert_eq!(json["shape"], serde_json::json!([]));
        assert_eq!(
            json["drift"][0],
            serde_json::json!({"month":"2026-04","energy":null,"valence":null,"bpm":null,"plays":1})
        );
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListeningStats {
    pub plays: i64,
    pub estimated_seconds: i64,
    pub distinct_tracks: i64,
    pub distinct_artists: i64,
    pub distinct_albums: i64,
    pub first_play: Option<String>,
    pub last_play: Option<String>,
    pub by_source: std::collections::BTreeMap<String, i64>,
    pub clock: [[i64; 24]; 7],
    pub peak: Option<Peak>,
    pub shape: Vec<ShapeCell>,
    pub drift: Vec<DriftMonth>,
    pub top_artists: Vec<TopArtist>,
    pub top_tracks: Vec<TopTrack>,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct Peak {
    pub weekday: i32,
    pub hour: i32,
    pub plays: i64,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct ShapeCell {
    pub x: i32,
    pub y: i32,
    pub plays: i64,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct DriftMonth {
    pub month: String,
    pub energy: Option<f64>,
    pub valence: Option<f64>,
    pub bpm: Option<f64>,
    pub plays: i64,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TopArtist {
    pub id: String,
    pub name: String,
    pub plays: i64,
    pub estimated_seconds: i64,
}
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TopTrack {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub plays: i64,
}
