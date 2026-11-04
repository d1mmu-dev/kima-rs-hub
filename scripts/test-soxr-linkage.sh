#!/bin/sh
# Real ELF loading plus controlled ldd diagnostics for unavailable-host cases.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
work=$(mktemp -d "${TMPDIR:?set TMPDIR to a scratch directory}/soxr-linkage.XXXXXXXX")
trap 'rm -rf "$work"' EXIT HUP INT TERM
mkdir -p "$work/prefix/lib" "$work/bin"
printf 'int soxr_probe(void) { return 0; }\n' > "$work/soxr.c"
printf 'extern int soxr_probe(void); int main(void) { return soxr_probe(); }\n' > "$work/main.c"
cc -shared -fPIC -Wl,-soname,libsoxr.so.0 -Wl,--no-as-needed -lgomp "$work/soxr.c" -o "$work/prefix/lib/libsoxr.so.0"
cc "$work/main.c" -L"$work/prefix/lib" -l:libsoxr.so.0 -o "$work/probe"
export LD_LIBRARY_PATH="$work/prefix/lib"
check() { sh "$root/scripts/check-soxr-linkage.sh" "$work/probe" "$1"; }
check "$work/prefix"
echo 'PASS real resolved ELF'
if check "$work/wrong"; then echo 'FAIL wrong prefix accepted' >&2; exit 1; fi
echo 'PASS wrong prefix rejected'
# Force a real missing libgomp without removing/modifying the host library:
# an absolute DT_NEEDED name cannot fall back to the system search path.
printf 'int gomp_probe(void) { return 0; }\n' > "$work/gomp.c"
cc -shared -fPIC -Wl,-soname,"$work/libgomp.so.1" "$work/gomp.c" -o "$work/libgomp.so.1"
cc -shared -fPIC -Wl,-soname,libsoxr.so.0 -Wl,--no-as-needed "$work/libgomp.so.1" "$work/soxr.c" -o "$work/prefix/lib/libsoxr.so.0"
mv "$work/libgomp.so.1" "$work/hidden-gomp.so"
if check "$work/prefix"; then echo 'FAIL real unresolved gomp accepted' >&2; exit 1; fi
echo 'PASS real missing gomp rejected'
# A real unresolved transitive dependency must fail even if soxr/gomp resolve.
printf 'int missing_probe(void) { return 0; }\n' > "$work/missing.c"
cc -shared -fPIC -Wl,-soname,libkima_missing.so.1 "$work/missing.c" -o "$work/prefix/lib/libkima_missing.so.1"
cc -shared -fPIC -Wl,-soname,libsoxr.so.0 -Wl,--no-as-needed -lgomp -L"$work/prefix/lib" -l:libkima_missing.so.1 "$work/soxr.c" -o "$work/prefix/lib/libsoxr.so.0"
mv "$work/prefix/lib/libkima_missing.so.1" "$work/hidden.so"
if check "$work/prefix"; then echo 'FAIL unresolved transitive dependency accepted' >&2; exit 1; fi
echo 'PASS real missing transitive dependency rejected'
# The system gomp cannot safely be removed: supply exact loader diagnostics.
printf '#!/bin/sh\nprintf "%%s\\n" "$LDD_DIAGNOSTIC"\n' > "$work/bin/ldd"
chmod +x "$work/bin/ldd"
PATH="$work/bin:$PATH"
export PATH
for destination in 'not found' 'relative/libgomp.so.1'; do
    LDD_DIAGNOSTIC="libsoxr.so.0 => $work/prefix/lib/libsoxr.so.0 (0x1234)
libgomp.so.1 => $destination (0x5678)"
    export LDD_DIAGNOSTIC
    if check "$work/prefix"; then echo "FAIL gomp $destination accepted" >&2; exit 1; fi
    echo "PASS gomp $destination rejected"
done
LDD_DIAGNOSTIC="libsoxr.so.0 => $work/prefix/lib/libsoxr.so.0 (0x1234)
libgomp.so.1 => /usr/lib/libgomp.so.1 (0x5678)"
export LDD_DIAGNOSTIC
check "$work/prefix"
echo 'PASS absolute gomp diagnostic'
