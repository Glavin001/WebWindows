/* DirectInput the way games of the era use it: create the interface, look
 * for joysticks (none here), and read the keyboard and mouse as devices.
 *
 * libs: -ldinput -ldxguid -luser32
 */
#define DIRECTINPUT_VERSION 0x0700
#include <windows.h>
#include <dinput.h>
#include <stdio.h>

static BOOL CALLBACK count(const DIDEVICEINSTANCEA *inst, void *ctx)
{
    ++*(int *)ctx;
    return DIENUM_CONTINUE;
}

int main(void)
{
    WNDCLASSA wc = { 0, DefWindowProcA, 0, 0, NULL, NULL, NULL, NULL, NULL, "dinput-test" };
    IDirectInputA *di;
    IDirectInputDeviceA *kb, *mouse;
    BYTE keys[256];
    DIMOUSESTATE ms;
    HWND hwnd;
    HRESULT hr;
    int joysticks = 0, keyboards = 0;

    setvbuf(stdout, NULL, _IONBF, 0);
    wc.hInstance = GetModuleHandleA(NULL);
    RegisterClassA(&wc);
    hwnd = CreateWindowA("dinput-test", "dinput", WS_OVERLAPPEDWINDOW | WS_VISIBLE, 0, 0, 200, 150, NULL, NULL,
                         wc.hInstance, NULL);
    SetForegroundWindow(hwnd);

    hr = DirectInputCreateA(wc.hInstance, DIRECTINPUT_VERSION, &di, NULL);
    printf("DirectInputCreate: %08lx\n", hr);
    if (FAILED(hr)) return 1;
    hr = IDirectInput_EnumDevices(di, DIDEVTYPE_JOYSTICK, count, &joysticks, DIEDFL_ATTACHEDONLY);
    printf("EnumDevices joysticks: %08lx\n", hr);
    IDirectInput_EnumDevices(di, DIDEVTYPE_KEYBOARD, count, &keyboards, DIEDFL_ATTACHEDONLY);
    printf("a keyboard: %d\n", keyboards > 0);

    hr = IDirectInput_CreateDevice(di, &GUID_SysKeyboard, &kb, NULL);
    printf("keyboard device: %08lx\n", hr);
    printf("SetDataFormat: %08lx\n", IDirectInputDevice_SetDataFormat(kb, &c_dfDIKeyboard));
    printf("SetCooperativeLevel: %08lx\n",
           IDirectInputDevice_SetCooperativeLevel(kb, hwnd, DISCL_NONEXCLUSIVE | DISCL_BACKGROUND));
    printf("Acquire: %08lx\n", IDirectInputDevice_Acquire(kb));
    memset(keys, 0xcc, sizeof(keys));
    hr = IDirectInputDevice_GetDeviceState(kb, sizeof(keys), keys);
    printf("GetDeviceState: %08lx, no key down: %d\n", hr, !keys[DIK_ESCAPE] && !keys[DIK_SPACE]);
    IDirectInputDevice_Unacquire(kb);
    IDirectInputDevice_Release(kb);

    hr = IDirectInput_CreateDevice(di, &GUID_SysMouse, &mouse, NULL);
    printf("mouse device: %08lx\n", hr);
    IDirectInputDevice_SetDataFormat(mouse, &c_dfDIMouse);
    IDirectInputDevice_SetCooperativeLevel(mouse, hwnd, DISCL_NONEXCLUSIVE | DISCL_BACKGROUND);
    printf("Acquire: %08lx\n", IDirectInputDevice_Acquire(mouse));
    hr = IDirectInputDevice_GetDeviceState(mouse, sizeof(ms), &ms);
    printf("GetDeviceState: %08lx\n", hr);
    IDirectInputDevice_Release(mouse);

    IDirectInput_Release(di);
    DestroyWindow(hwnd);
    printf("done\n");
    return 0;
}
