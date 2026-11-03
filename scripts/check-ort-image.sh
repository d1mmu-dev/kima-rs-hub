#!/bin/sh
# Run matching backend-built integration binaries in the unmodified final image.
# Binaries are test-only read-only mounts, not shipped in the production image.
set -eu
if [ "$#" -ne 2 ]; then
    echo "usage: $0 IMAGE ABSOLUTE_TEST_BINARY_DIRECTORY" >&2
    exit 1
fi
case "$2" in /*) ;; *) echo 'test binary directory must be absolute' >&2; exit 1 ;; esac
docker run --rm --network none --read-only \
    --tmpfs /tmp:rw,noexec,nosuid,size=32m --memory 512m --cpus 2 --pids-limit 64 \
    --user kima -v "$2:/checks:ro" --entrypoint sh "$1" -c '
set -eu
test "$(id -u)" != 0
for path in /opt /opt/kima /opt/kima/ort "$ORT_DYLIB_PATH"; do
    test ! -w "$path"
    test "$(stat -c %u "$path")" = 0
done
test -s /opt/kima/ort/LICENSE
test -s /opt/kima/ort/ThirdPartyNotices.txt
printf "%s  %s\n" 7520bc1b4f649ee8fa956442a66ded303e8fd3b465b0159b7e893bdc87df4e38 "$ORT_DYLIB_PATH" | sha256sum -c -
ldd "$ORT_DYLIB_PATH"
/checks/onnx_session --nocapture
/checks/onnx_runtime --nocapture
/checks/onnx_resources --nocapture
'
