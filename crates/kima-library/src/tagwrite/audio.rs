//! The audio check (spec §19.8): an MD5 of the first audio stream's packets, as ffmpeg's `md5`
//! muxer computes it with `-c copy`. Tags (and an embedded cover, a separate stream) are not
//! part of it, so a tag write leaves it unchanged and anything that touches the audio changes it.

use std::ffi::OsString;
use std::path::Path;
use std::process::{Command, Stdio};

use super::TagWriteError;

/// `ffmpeg -v error -i file:<abs> -map 0:a:0 -c copy -f md5 -`, parsing `MD5=<hex>`.
///
/// ffmpeg always gets `file:` and the absolute path, as raw OS bytes: a name that starts with
/// `-`, contains `:` (which ffmpeg would read as a protocol) or is not UTF-8 is opened like any
/// other. Blocking.
pub fn audio_md5(path: &Path) -> Result<[u8; 16], TagWriteError> {
    let abs = std::path::absolute(path)?;
    let mut input = OsString::from("file:");
    input.push(abs.as_os_str());
    let out = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&input)
        .args(["-map", "0:a:0", "-c", "copy", "-f", "md5", "-"])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| TagWriteError::Io(format!("cannot run ffmpeg: {e}")))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(TagWriteError::Io(format!(
            "ffmpeg could not read the audio: {}",
            err.trim()
        )));
    }
    parse_md5(&String::from_utf8_lossy(&out.stdout))
        .ok_or_else(|| TagWriteError::Io("ffmpeg printed no audio MD5".to_string()))
}

fn parse_md5(stdout: &str) -> Option<[u8; 16]> {
    let hex_part = stdout.lines().find_map(|l| l.trim().strip_prefix("MD5="))?;
    let mut out = [0u8; 16];
    hex::decode_to_slice(hex_part.trim(), &mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::parse_md5;

    #[test]
    fn parses_the_md5_line_only() {
        let got = parse_md5("MD5=000102030405060708090a0b0c0d0e0f\n").unwrap();
        assert_eq!(got[15], 15);
        assert_eq!(parse_md5("MD5=xyz\n"), None);
        assert_eq!(parse_md5("MD5=0001\n"), None);
        assert_eq!(parse_md5(""), None);
    }
}
