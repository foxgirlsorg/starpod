#!/usr/bin/env bash
# Cross-compile the Windows release binary inside the Dockerfile.windows image.
# Output: /work/dist/starpod.exe (i.e. <repo>/dist/starpod.exe on the host).
set -euo pipefail

TARGET=x86_64-pc-windows-gnu
TARGET_DIR="${CARGO_TARGET_DIR:-/target-win}"
cd /work

cargo build --release -p starpod-app --target "$TARGET"

mkdir -p dist
cp -f "$TARGET_DIR/$TARGET/release/starpod.exe" dist/starpod.exe

# List the DLLs the exe imports. Anything that is not a Windows system DLL
# (KERNEL32, USER32, api-ms-win-*, ...) is a mingw runtime DLL that must ship
# beside the exe, so copy it from the mingw sysroot.
DLLS=$(x86_64-w64-mingw32-objdump -p dist/starpod.exe | awk '/DLL Name:/ {print $3}' | sort -fu)
echo "DLL imports of dist/starpod.exe:"
echo "$DLLS" | sed 's/^/  /'

# Windows system DLLs: api-ms-win-* / ext-ms-* API sets, plus the OS DLLs
# that ship with every Windows install.
SYSTEM_API_SET='^(api-ms-win-|ext-ms-)'
SYSTEM_DLL='^(kernel32|user32|gdi32|advapi32|shell32|ole32|oleaut32|ws2_32|bcrypt|bcryptprimitives|crypt32|secur32|shlwapi|userenv|ntdll|msvcrt|imm32|version|winmm|opengl32|dwmapi|dwrite|uxtheme|comdlg32|comctl32|setupapi|cfgmgr32|iphlpapi|mswsock|rpcrt4|wininet|winhttp|psapi|dbghelp|powrprof|hid|dxgi|d3d11|d3d12|d3d9|ucrtbase|cryptsp|sechost|wtsapi32|netapi32|ncrypt|normaliz|propsys|tbs|winspool|d2d1|dcomp|combase)\.dll$'
SYSROOT_BIN=$(dirname "$(x86_64-w64-mingw32-gcc -print-libgcc-file-name)")
for dll in $DLLS; do
    lower=$(echo "$dll" | tr 'A-Z' 'a-z')
    if echo "$lower" | grep -Eq "$SYSTEM_API_SET" || echo "$lower" | grep -Eq "$SYSTEM_DLL"; then
        continue
    fi
    found=$(find "$SYSROOT_BIN" /usr/x86_64-w64-mingw32/lib -iname "$dll" 2>/dev/null | head -n1 || true)
    if [ -n "$found" ]; then
        cp -f "$found" dist/
        echo "copied runtime DLL beside exe: $dll"
    else
        echo "WARNING: non-system DLL $dll not found in mingw sysroot" >&2
    fi
done

ls -l dist/
