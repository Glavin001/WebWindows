/* Direct3D 9 on translated Wine: a window, a device, one frame with a
 * clear, a triangle drawn with vs_2_0/ps_2_0 shaders (and shader
 * constants), and a pre-transformed quad drawn with the fixed-function
 * pipeline; then Present. Prints the result of each step.
 *
 * Window at (40,30), client area 320x240. In the client area: blue
 * background, a triangle with red, green and blue corners shifted right by
 * a vertex shader constant and halved in green by a pixel shader constant,
 * and a yellow square at (10,10)-(60,60).
 *
 * The shaders were compiled with vkd3d-compiler 1.19 from:
 *
 *   float4 offset : register(c0);
 *   struct VSOut { float4 pos : POSITION; float4 color : COLOR0; };
 *   VSOut vs(float4 pos : POSITION, float4 color : COLOR0)
 *   { VSOut o; o.pos = pos + offset; o.color = color; return o; }
 *   float4 tint : register(c0);
 *   float4 ps(float4 color : COLOR0) : COLOR { return color * tint; }
 */
#include <windows.h>
#include <d3d9.h>
#include <stdio.h>

static const DWORD vs_code[] = {
    0xfffe0200, 0x0018fffe, 0x42415443, 0x0000001c, 0x00000048, 0xfffe0200,
    0x00000001, 0x0000001c, 0x00000000, 0x00000000, 0x00000030, 0x00000002,
    0x00000001, 0x00000038, 0x00000000, 0x7366666f, 0xab007465, 0x00030001,
    0x00040001, 0x00000001, 0x00000001, 0x33646b76, 0x68732d64, 0x72656461,
    0x312e3120, 0xabab0039, 0x0200001f, 0x80000000, 0x900f0000, 0x0200001f,
    0x8000000a, 0x900f0001, 0x03000002, 0x800f0000, 0x90e40000, 0xa0e40000,
    0x02000001, 0xc00f0000, 0x80e40000, 0x02000001, 0xd00f0000, 0x90e40001,
    0x0000ffff,
};

static const DWORD ps_code[] = {
    0xffff0200, 0x0018fffe, 0x42415443, 0x0000001c, 0x00000048, 0xffff0200,
    0x00000001, 0x0000001c, 0x00000000, 0x00000000, 0x00000030, 0x00000002,
    0x00000001, 0x00000038, 0x00000000, 0x746e6974, 0xababab00, 0x00030001,
    0x00040001, 0x00000001, 0x00000001, 0x33646b76, 0x68732d64, 0x72656461,
    0x312e3120, 0xabab0039, 0x0200001f, 0x80000000, 0x900f0000, 0x03000005,
    0x800f0000, 0x90e40000, 0xa0e40000, 0x02000001, 0x800f0800, 0x80e40000,
    0x0000ffff,
};

struct vertex
{
    float x, y, z, w;
    D3DCOLOR color;
};

static LRESULT CALLBACK proc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_DESTROY)
    {
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcA(hwnd, msg, wp, lp);
}

#define CHECK(what, expr) do { HRESULT hr_ = (expr); printf("%s: %#lx\n", what, hr_); if (FAILED(hr_)) return 1; } while (0)

int main(void)
{
    IDirect3D9 *(WINAPI *create)(UINT);
    IDirect3DVertexDeclaration9 *decl;
    D3DPRESENT_PARAMETERS pp = {0};
    IDirect3DPixelShader9 *ps;
    IDirect3DVertexShader9 *vs;
    IDirect3DDevice9 *device;
    D3DADAPTER_IDENTIFIER9 id;
    WNDCLASSA wc = {0};
    RECT rc = {0, 0, 320, 240};
    IDirect3D9 *d3d;
    HMODULE module;
    HWND hwnd;
    MSG msg;

    static const D3DVERTEXELEMENT9 elements[] =
    {
        {0, 0, D3DDECLTYPE_FLOAT4, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
        {0, 16, D3DDECLTYPE_D3DCOLOR, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_COLOR, 0},
        D3DDECL_END()
    };
    /* Clip space; the vertex shader adds 0.25 to x. */
    static const struct vertex tri[] =
    {
        {-0.75f, -0.75f, 0.5f, 1.0f, 0xffff0000},
        { 0.0f,   0.75f, 0.5f, 1.0f, 0xff00ff00},
        { 0.75f, -0.75f, 0.5f, 1.0f, 0xff0000ff},
    };
    /* Screen space (D3DFVF_XYZRHW), drawn without shaders. */
    static const struct vertex quad[] =
    {
        {10.0f, 10.0f, 0.5f, 1.0f, 0xffffff00},
        {60.0f, 10.0f, 0.5f, 1.0f, 0xffffff00},
        {10.0f, 60.0f, 0.5f, 1.0f, 0xffffff00},
        {60.0f, 60.0f, 0.5f, 1.0f, 0xffffff00},
    };
    static const float offset[4] = {0.25f, 0.0f, 0.0f, 0.0f};
    static const float tint[4] = {1.0f, 0.5f, 1.0f, 1.0f};

    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.hCursor = LoadCursorA(NULL, (LPCSTR)IDC_ARROW);
    wc.lpszClassName = "d3d9tri";
    RegisterClassA(&wc);
    AdjustWindowRect(&rc, WS_OVERLAPPEDWINDOW, FALSE);
    hwnd = CreateWindowA("d3d9tri", "Direct3D 9", WS_OVERLAPPEDWINDOW, 40, 30,
            rc.right - rc.left, rc.bottom - rc.top, NULL, NULL, wc.hInstance, NULL);
    ShowWindow(hwnd, SW_SHOW);
    UpdateWindow(hwnd);

    if (!(module = LoadLibraryA("d3d9.dll")))
    {
        printf("LoadLibrary(d3d9): error %lu\n", GetLastError());
        return 1;
    }
    create = (void *)GetProcAddress(module, "Direct3DCreate9");
    if (!(d3d = create(D3D_SDK_VERSION)))
    {
        printf("Direct3DCreate9 failed\n");
        return 1;
    }
    if (SUCCEEDED(IDirect3D9_GetAdapterIdentifier(d3d, D3DADAPTER_DEFAULT, 0, &id)))
        printf("adapter: %s (%s)\n", id.Description, id.Driver);

    pp.Windowed = TRUE;
    pp.SwapEffect = D3DSWAPEFFECT_DISCARD;
    pp.BackBufferFormat = D3DFMT_X8R8G8B8;
    pp.BackBufferWidth = 320;
    pp.BackBufferHeight = 240;
    pp.hDeviceWindow = hwnd;
    CHECK("CreateDevice", IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd,
            D3DCREATE_HARDWARE_VERTEXPROCESSING, &pp, &device));

    CHECK("CreateVertexDeclaration", IDirect3DDevice9_CreateVertexDeclaration(device, elements, &decl));
    CHECK("CreateVertexShader", IDirect3DDevice9_CreateVertexShader(device, vs_code, &vs));
    CHECK("CreatePixelShader", IDirect3DDevice9_CreatePixelShader(device, ps_code, &ps));

    CHECK("Clear", IDirect3DDevice9_Clear(device, 0, NULL, D3DCLEAR_TARGET, D3DCOLOR_XRGB(0, 0, 128), 1.0f, 0));
    CHECK("BeginScene", IDirect3DDevice9_BeginScene(device));
    IDirect3DDevice9_SetRenderState(device, D3DRS_LIGHTING, FALSE);
    IDirect3DDevice9_SetRenderState(device, D3DRS_CULLMODE, D3DCULL_NONE);

    IDirect3DDevice9_SetVertexDeclaration(device, decl);
    IDirect3DDevice9_SetVertexShader(device, vs);
    IDirect3DDevice9_SetPixelShader(device, ps);
    IDirect3DDevice9_SetVertexShaderConstantF(device, 0, offset, 1);
    IDirect3DDevice9_SetPixelShaderConstantF(device, 0, tint, 1);
    CHECK("DrawPrimitiveUP (shaders)", IDirect3DDevice9_DrawPrimitiveUP(device, D3DPT_TRIANGLELIST, 1, tri, sizeof(*tri)));

    IDirect3DDevice9_SetVertexShader(device, NULL);
    IDirect3DDevice9_SetPixelShader(device, NULL);
    IDirect3DDevice9_SetFVF(device, D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
    IDirect3DDevice9_SetTextureStageState(device, 0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
    IDirect3DDevice9_SetTextureStageState(device, 0, D3DTSS_COLORARG1, D3DTA_DIFFUSE);
    IDirect3DDevice9_SetTextureStageState(device, 0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
    IDirect3DDevice9_SetTextureStageState(device, 0, D3DTSS_ALPHAARG1, D3DTA_DIFFUSE);
    CHECK("DrawPrimitiveUP (fixed function)", IDirect3DDevice9_DrawPrimitiveUP(device, D3DPT_TRIANGLESTRIP, 2, quad, sizeof(*quad)));

    CHECK("EndScene", IDirect3DDevice9_EndScene(device));
    CHECK("Present", IDirect3DDevice9_Present(device, NULL, NULL, NULL, NULL));
    fflush(stdout);

    while (GetMessageA(&msg, NULL, 0, 0))
    {
        TranslateMessage(&msg);
        DispatchMessageA(&msg);
    }

    IDirect3DPixelShader9_Release(ps);
    IDirect3DVertexShader9_Release(vs);
    IDirect3DVertexDeclaration9_Release(decl);
    IDirect3DDevice9_Release(device);
    IDirect3D9_Release(d3d);
    return 0;
}
