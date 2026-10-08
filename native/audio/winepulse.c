/*
 * The audio driver DLL Wine's mmdevapi loads ("winepulse.drv", the first
 * name it tries). It has no code of its own: mmdevapi asks the kernel for
 * its Unix library and calls the driver functions through
 * __wine_unix_call, which the browser host answers (runtime/wine/audio.mjs).
 *
 * Built by tools/wine/build.sh winepulse.drv (Wine's own is disabled: the
 * build is configured without PulseAudio).
 */
#include <windows.h>

BOOL WINAPI DllMain( HINSTANCE instance, DWORD reason, void *reserved )
{
    if (reason == DLL_PROCESS_ATTACH) DisableThreadLibraryCalls( instance );
    return TRUE;
}
