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
