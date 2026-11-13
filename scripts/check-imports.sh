#!/bin/bash
# Fails if the given EXE imports any DLL outside the Windows system allowlist.
# Usage: scripts/check-imports.sh target/x86_64-pc-windows-gnu/release/linote.exe
set -euo pipefail
exe="${1:?exe path}"
allow='^(kernel32|user32|gdi32|imm32|ole32|oleaut32|advapi32|shell32|shlwapi|comctl32|comdlg32|msimg32|uxtheme|dwmapi|d2d1|dwrite|msvcrt|ntdll|ws2_32|userenv|propsys|bcrypt|bcryptprimitives|crypt32|version|winmm|uuid|combase|rpcrt4|api-ms-win-[a-z0-9-]+)\.dll$'
bad=0
while read -r dll; do
  lc=$(echo "$dll" | tr 'A-Z' 'a-z')
  if ! [[ "$lc" =~ $allow ]]; then echo "non-system DLL import: $dll"; bad=1; fi
done < <(x86_64-w64-mingw32-objdump -p "$exe" | sed -n 's/^\s*DLL Name: //p' | sort -u)
[ $bad -eq 0 ] && echo "imports ok: $exe"
exit $bad
