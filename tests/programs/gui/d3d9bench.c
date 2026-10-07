/* Direct3D 9 benchmark: a grid of spinning textured cubes, one draw call
 * each (as a Direct3D 9 game draws its objects), and a cloud of particles
 * streamed through a dynamic vertex buffer every frame and drawn with the
 * fixed-function pipeline and alpha blending. Renders as fast as it can and
 * prints, once a second, the frame rate and where each frame's time went:
 * "scene" is the program's Direct3D calls from Clear to EndScene, "present"
 * is Present.
 *
 *   d3d9bench [cubes [particles [seconds [materials [width height]]]]]
 *
 * Defaults: 400 cubes, 2000 particles, run until the window is closed
 * (seconds 0), 1 material, a 640x480 client area. With seconds, it stops
 * after that long and prints a summary line. With more than one material,
 * consecutive cubes use different ones, as a game's objects do: each draw
 * then also changes the texture, the sampler filter and (every other
 * material) alpha blending.
 *
 * The shaders were compiled with vkd3d-compiler 1.19 from:
 *
 *   float4 row0 : register(c0);   (rows of the cube's world-view-projection)
 *   float4 row1 : register(c1);
 *   float4 row2 : register(c2);
 *   float4 row3 : register(c3);
 *   float4 tint : register(c4);
 *   struct VSOut { float4 pos : POSITION; float4 color : COLOR0; float2 uv : TEXCOORD0; };
 *   VSOut vs(float4 pos : POSITION, float4 color : COLOR0, float2 uv : TEXCOORD0)
 *   {
 *       VSOut o;
 *       o.pos = float4(dot(pos, row0), dot(pos, row1), dot(pos, row2), dot(pos, row3));
 *       o.color = color * tint;
 *       o.uv = uv;
 *       return o;
 *   }
 *   sampler2D tex : register(s0);
 *   float4 ps(float4 color : COLOR0, float2 uv : TEXCOORD0) : COLOR
 *   { return tex2D(tex, uv) * color; }
 */
#include <windows.h>
#include <d3d9.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>

static const DWORD vs_code[] = {
    0xfffe0200, 0x0034fffe, 0x42415443, 0x0000001c, 0x000000b8, 0xfffe0200,
    0x00000005, 0x0000001c, 0x00000000, 0x00000000, 0x00000080, 0x00000002,
    0x00000001, 0x00000088, 0x00000000, 0x00000098, 0x00010002, 0x00000001,
    0x00000088, 0x00000000, 0x000000a0, 0x00020002, 0x00000001, 0x00000088,
    0x00000000, 0x000000a8, 0x00030002, 0x00000001, 0x00000088, 0x00000000,
    0x000000b0, 0x00040002, 0x00000001, 0x00000088, 0x00000000, 0x30776f72,
    0xababab00, 0x00030001, 0x00040001, 0x00000001, 0x00000001, 0x31776f72,
    0xababab00, 0x32776f72, 0xababab00, 0x33776f72, 0xababab00, 0x746e6974,
    0xababab00, 0x33646b76, 0x68732d64, 0x72656461, 0x312e3120, 0xabab0039,
    0x0200001f, 0x80000000, 0x900f0000, 0x0200001f, 0x8000000a, 0x900f0001,
    0x0200001f, 0x80000005, 0x900f0002, 0x03000009, 0x80010000, 0x90e40000,
    0xa0e40000, 0x03000009, 0x80020000, 0x90e40000, 0xa0e40001, 0x03000009,
    0x80040000, 0x90e40000, 0xa0e40002, 0x03000009, 0x80080000, 0x90e40000,
    0xa0e40003, 0x02000001, 0x80010001, 0x80000000, 0x02000001, 0x80020001,
    0x80550000, 0x02000001, 0x80040001, 0x80aa0000, 0x02000001, 0x80080001,
    0x80ff0000, 0x02000001, 0x800f0000, 0x80e40001, 0x03000005, 0x800f0001,
    0x90e40001, 0xa0e40004, 0x02000001, 0xc00f0000, 0x80e40000, 0x02000001,
    0xd00f0000, 0x80e40001, 0x02000001, 0xe0030000, 0x90040002, 0x0000ffff,
};

static const DWORD ps_code[] = {
    0xffff0200, 0x0017fffe, 0x42415443, 0x0000001c, 0x00000044, 0xffff0200,
    0x00000001, 0x0000001c, 0x00000000, 0x00000000, 0x00000030, 0x00000003,
    0x00000001, 0x00000034, 0x00000000, 0x00786574, 0x000c0004, 0x00010001,
    0x00000001, 0x00000001, 0x33646b76, 0x68732d64, 0x72656461, 0x312e3120,
    0xabab0039, 0x0200001f, 0x80000000, 0x900f0000, 0x0200001f, 0x80000000,
    0xb0030000, 0x0200001f, 0x90000000, 0xa00f0800, 0x03000042, 0x800f0000,
    0xb0040000, 0xa0e40800, 0x03000005, 0x800f0000, 0x80e40000, 0x90e40000,
    0x02000001, 0x800f0800, 0x80e40000, 0x0000ffff,
};

struct cube_vertex
{
    float x, y, z;
    D3DCOLOR color;
    float u, v;
};

struct particle_vertex
{
    float x, y, z, rhw;
    D3DCOLOR color;
};

static BOOL quit;

static LRESULT CALLBACK proc(HWND hwnd, UINT msg, WPARAM wp, LPARAM lp)
{
    if (msg == WM_DESTROY)
    {
        quit = TRUE;
        PostQuitMessage(0);
        return 0;
    }
    return DefWindowProcA(hwnd, msg, wp, lp);
}

#define CHECK(what, expr) do { HRESULT hr_ = (expr); if (FAILED(hr_)) { printf("%s: %#lx\n", what, hr_); return 1; } } while (0)

static double now_ms(void)
{
    static LARGE_INTEGER freq;
    LARGE_INTEGER t;
    if (!freq.QuadPart) QueryPerformanceFrequency(&freq);
    QueryPerformanceCounter(&t);
    return (double)t.QuadPart * 1000.0 / (double)freq.QuadPart;
}

/* A cube with a colour per face, 24 vertices and 36 indices. */
static void make_cube(struct cube_vertex *v, WORD *idx)
{
    static const float n[6][3] = {{0,0,-1}, {0,0,1}, {-1,0,0}, {1,0,0}, {0,1,0}, {0,-1,0}};
    static const D3DCOLOR colors[6] = {0xffff8080, 0xff80ff80, 0xff8080ff, 0xffffff80, 0xffff80ff, 0xff80ffff};
    static const float uv[4][2] = {{0,0}, {1,0}, {0,1}, {1,1}};
    int f, k;

    for (f = 0; f < 6; ++f)
    {
        /* Two axes across the face. */
        float a[3] = {n[f][1], n[f][2], n[f][0]}, b[3];
        b[0] = n[f][1] * a[2] - n[f][2] * a[1];
        b[1] = n[f][2] * a[0] - n[f][0] * a[2];
        b[2] = n[f][0] * a[1] - n[f][1] * a[0];
        for (k = 0; k < 4; ++k)
        {
            float s = (k & 1) ? 1.0f : -1.0f, t = (k & 2) ? 1.0f : -1.0f;
            struct cube_vertex *p = &v[f * 4 + k];
            p->x = 0.7f * (n[f][0] + s * a[0] + t * b[0]);
            p->y = 0.7f * (n[f][1] + s * a[1] + t * b[1]);
            p->z = 0.7f * (n[f][2] + s * a[2] + t * b[2]);
            p->color = colors[f];
            p->u = uv[k][0];
            p->v = uv[k][1];
        }
        idx[f * 6 + 0] = f * 4 + 0;
        idx[f * 6 + 1] = f * 4 + 1;
        idx[f * 6 + 2] = f * 4 + 2;
        idx[f * 6 + 3] = f * 4 + 2;
        idx[f * 6 + 4] = f * 4 + 1;
        idx[f * 6 + 5] = f * 4 + 3;
    }
}

int main(int argc, char **argv)
{
    int cubes = argc > 1 ? atoi(argv[1]) : 400;
    int particles = argc > 2 ? atoi(argv[2]) : 2000;
    double seconds = argc > 3 ? atof(argv[3]) : 0.0;
    int materials = argc > 4 ? atoi(argv[4]) : 1;
    int width = argc > 6 ? atoi(argv[5]) : 640;
    int height = argc > 6 ? atoi(argv[6]) : 480;
    IDirect3D9 *(WINAPI *create)(UINT);
    IDirect3DVertexDeclaration9 *decl;
    IDirect3DVertexBuffer9 *vb, *pvb = NULL;
    IDirect3DIndexBuffer9 *ib;
    D3DPRESENT_PARAMETERS pp = {0};
    IDirect3DPixelShader9 *ps;
    IDirect3DVertexShader9 *vs;
    IDirect3DDevice9 *device;
    IDirect3DTexture9 *tex[16];
    D3DADAPTER_IDENTIFIER9 id;
    D3DLOCKED_RECT lr;
    WNDCLASSA wc = {0};
    RECT rc = {0, 0, width, height};
    IDirect3D9 *d3d;
    HMODULE module;
    HWND hwnd;
    MSG msg;
    void *data;
    int i, x, y, grid, rows;
    double start, last, scene_ms = 0, present_ms = 0, worst_ms = 0;
    double total_scene = 0, total_present = 0;
    long frames = 0, total_frames = 0;
    float dist, f, aspect, zn = 0.5f, zf, qa, qb;

    static const D3DVERTEXELEMENT9 elements[] =
    {
        {0, 0, D3DDECLTYPE_FLOAT3, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_POSITION, 0},
        {0, 12, D3DDECLTYPE_D3DCOLOR, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_COLOR, 0},
        {0, 16, D3DDECLTYPE_FLOAT2, D3DDECLMETHOD_DEFAULT, D3DDECLUSAGE_TEXCOORD, 0},
        D3DDECL_END()
    };

    if (cubes < 0) cubes = 0;
    if (materials < 1) materials = 1;
    if (materials > 16) materials = 16;
    if (particles < 0) particles = 0;
    if (width < 64 || height < 64) width = 640, height = 480;
    printf("d3d9bench: %d cubes, %d particles, %d materials, %dx%d\n", cubes, particles, materials, width, height);

    wc.lpfnWndProc = proc;
    wc.hInstance = GetModuleHandleA(NULL);
    wc.hCursor = LoadCursorA(NULL, (LPCSTR)IDC_ARROW);
    wc.lpszClassName = "d3d9bench";
    RegisterClassA(&wc);
    AdjustWindowRect(&rc, WS_OVERLAPPEDWINDOW, FALSE);
    hwnd = CreateWindowA("d3d9bench", "Direct3D 9 benchmark", WS_OVERLAPPEDWINDOW, 10, 10,
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
    pp.BackBufferWidth = width;
    pp.BackBufferHeight = height;
    pp.EnableAutoDepthStencil = TRUE;
    pp.AutoDepthStencilFormat = D3DFMT_D24S8;
    pp.PresentationInterval = D3DPRESENT_INTERVAL_IMMEDIATE;
    pp.hDeviceWindow = hwnd;
    CHECK("CreateDevice", IDirect3D9_CreateDevice(d3d, D3DADAPTER_DEFAULT, D3DDEVTYPE_HAL, hwnd,
            D3DCREATE_HARDWARE_VERTEXPROCESSING, &pp, &device));

    CHECK("CreateVertexDeclaration", IDirect3DDevice9_CreateVertexDeclaration(device, elements, &decl));
    CHECK("CreateVertexShader", IDirect3DDevice9_CreateVertexShader(device, vs_code, &vs));
    CHECK("CreatePixelShader", IDirect3DDevice9_CreatePixelShader(device, ps_code, &ps));

    /* The cube, in static buffers. */
    CHECK("CreateVertexBuffer", IDirect3DDevice9_CreateVertexBuffer(device, 24 * sizeof(struct cube_vertex),
            D3DUSAGE_WRITEONLY, 0, D3DPOOL_MANAGED, &vb, NULL));
    CHECK("CreateIndexBuffer", IDirect3DDevice9_CreateIndexBuffer(device, 36 * sizeof(WORD),
            D3DUSAGE_WRITEONLY, D3DFMT_INDEX16, D3DPOOL_MANAGED, &ib, NULL));
    {
        struct cube_vertex verts[24];
        WORD idx[36];
        make_cube(verts, idx);
        CHECK("Lock vb", IDirect3DVertexBuffer9_Lock(vb, 0, 0, &data, 0));
        memcpy(data, verts, sizeof(verts));
        IDirect3DVertexBuffer9_Unlock(vb);
        CHECK("Lock ib", IDirect3DIndexBuffer9_Lock(ib, 0, 0, &data, 0));
        memcpy(data, idx, sizeof(idx));
        IDirect3DIndexBuffer9_Unlock(ib);
    }

    /* A 64x64 checkerboard per material, its squares 2 to 32 texels. */
    for (i = 0; i < materials; ++i)
    {
        CHECK("CreateTexture", IDirect3DDevice9_CreateTexture(device, 64, 64, 1, 0, D3DFMT_A8R8G8B8,
                D3DPOOL_MANAGED, &tex[i], NULL));
        CHECK("LockRect", IDirect3DTexture9_LockRect(tex[i], 0, &lr, NULL, 0));
        for (y = 0; y < 64; ++y)
            for (x = 0; x < 64; ++x)
            {
                int shift = 1 + (i + 2) % 5;
                BYTE v = ((x >> shift) ^ (y >> shift)) & 1 ? 255 : 96;
                ((DWORD *)((BYTE *)lr.pBits + y * lr.Pitch))[x] = D3DCOLOR_ARGB(i & 1 ? 192 : 255, v, v, v);
            }
        IDirect3DTexture9_UnlockRect(tex[i], 0);
    }

    if (particles)
        CHECK("CreateVertexBuffer (particles)", IDirect3DDevice9_CreateVertexBuffer(device,
                particles * 6 * sizeof(struct particle_vertex), D3DUSAGE_DYNAMIC | D3DUSAGE_WRITEONLY,
                D3DFVF_XYZRHW | D3DFVF_DIFFUSE, D3DPOOL_DEFAULT, &pvb, NULL));
    printf("draws per frame: %d\n", cubes + (particles ? 1 : 0));
    fflush(stdout);

    /* A camera far enough back to see the whole grid. */
    for (grid = 1; grid * grid < cubes; ++grid);
    rows = (cubes + grid - 1) / grid;
    dist = grid * 2.2f + 2.0f;
    zf = dist * 2.0f + 4.0f;
    f = 1.0f / tanf(3.14159265f / 6.0f);
    aspect = (float)width / (float)height;
    qa = zf / (zf - zn);
    qb = -zn * zf / (zf - zn);

    start = last = now_ms();
    while (!quit)
    {
        double t0, t1, t2, t;

        while (PeekMessageA(&msg, NULL, 0, 0, PM_REMOVE))
        {
            TranslateMessage(&msg);
            DispatchMessageA(&msg);
        }
        if (quit) break;

        t0 = now_ms();
        t = (t0 - start) / 1000.0;
        IDirect3DDevice9_Clear(device, 0, NULL, D3DCLEAR_TARGET | D3DCLEAR_ZBUFFER,
                D3DCOLOR_XRGB(16, 24, 48), 1.0f, 0);
        IDirect3DDevice9_BeginScene(device);

        /* Cubes: shaders, a texture, depth; per cube its matrix and tint. */
        IDirect3DDevice9_SetRenderState(device, D3DRS_ZENABLE, D3DZB_TRUE);
        IDirect3DDevice9_SetRenderState(device, D3DRS_ALPHABLENDENABLE, FALSE);
        IDirect3DDevice9_SetRenderState(device, D3DRS_CULLMODE, D3DCULL_NONE);
        IDirect3DDevice9_SetRenderState(device, D3DRS_LIGHTING, FALSE);
        IDirect3DDevice9_SetVertexDeclaration(device, decl);
        IDirect3DDevice9_SetVertexShader(device, vs);
        IDirect3DDevice9_SetPixelShader(device, ps);
        IDirect3DDevice9_SetTexture(device, 0, (IDirect3DBaseTexture9 *)tex[0]);
        IDirect3DDevice9_SetSamplerState(device, 0, D3DSAMP_MINFILTER, D3DTEXF_LINEAR);
        IDirect3DDevice9_SetSamplerState(device, 0, D3DSAMP_MAGFILTER, D3DTEXF_LINEAR);
        IDirect3DDevice9_SetRenderState(device, D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
        IDirect3DDevice9_SetRenderState(device, D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
        IDirect3DDevice9_SetStreamSource(device, 0, vb, 0, sizeof(struct cube_vertex));
        IDirect3DDevice9_SetIndices(device, ib);
        for (i = 0; i < cubes; ++i)
        {
            float ay = (float)t * 1.3f + i * 0.37f, ax = (float)t * 0.7f + i * 0.21f;
            float cy = cosf(ay), sy = sinf(ay), cx = cosf(ax), sx = sinf(ax);
            /* R = Ry * Rx; the cube's centre in view space. */
            float r[3][3] = {{cy, sy * sx, sy * cx}, {0, cx, -sx}, {-sy, cy * sx, cy * cx}};
            float tx = ((i % grid) - (grid - 1) * 0.5f) * 2.2f;
            float ty = ((i / grid) - (rows - 1) * 0.5f) * 2.2f;
            float tz = dist;
            float c[5][4] =
            {
                {f / aspect * r[0][0], f / aspect * r[0][1], f / aspect * r[0][2], f / aspect * tx},
                {f * r[1][0], f * r[1][1], f * r[1][2], f * ty},
                {qa * r[2][0], qa * r[2][1], qa * r[2][2], qa * tz + qb},
                {r[2][0], r[2][1], r[2][2], tz},
                {0.6f + 0.4f * sinf(i * 0.5f), 0.6f + 0.4f * sinf(i * 0.7f + 2.0f), 0.6f + 0.4f * sinf(i * 0.9f + 4.0f), 1.0f},
            };
            if (materials > 1)
            {
                int m = i % materials;
                IDirect3DDevice9_SetTexture(device, 0, (IDirect3DBaseTexture9 *)tex[m]);
                IDirect3DDevice9_SetSamplerState(device, 0, D3DSAMP_MAGFILTER, m & 2 ? D3DTEXF_POINT : D3DTEXF_LINEAR);
                IDirect3DDevice9_SetRenderState(device, D3DRS_ALPHABLENDENABLE, m & 1);
            }
            IDirect3DDevice9_SetVertexShaderConstantF(device, 0, &c[0][0], 5);
            IDirect3DDevice9_DrawIndexedPrimitive(device, D3DPT_TRIANGLELIST, 0, 0, 24, 0, 12);
        }

        /* Particles: written every frame, fixed function, blended. */
        if (particles && SUCCEEDED(IDirect3DVertexBuffer9_Lock(pvb, 0, 0, &data, D3DLOCK_DISCARD)))
        {
            struct particle_vertex *p = data;
            for (i = 0; i < particles; ++i, p += 6)
            {
                float a = (float)t * (0.3f + (i % 7) * 0.05f) + i * 2.399f;
                float rad = (0.1f + 0.4f * (float)((i * 7919) % 1000) / 1000.0f) * height;
                float px = width * 0.5f + cosf(a) * rad, py = height * 0.5f + sinf(a) * rad * 0.8f;
                D3DCOLOR col = D3DCOLOR_ARGB(160, 128 + (i * 37) % 128, 128 + (i * 61) % 128, 255);
                struct particle_vertex q[4] =
                {
                    {px - 2, py - 2, 0, 1, col}, {px + 2, py - 2, 0, 1, col},
                    {px - 2, py + 2, 0, 1, col}, {px + 2, py + 2, 0, 1, col},
                };
                p[0] = q[0]; p[1] = q[1]; p[2] = q[2];
                p[3] = q[2]; p[4] = q[1]; p[5] = q[3];
            }
            IDirect3DVertexBuffer9_Unlock(pvb);
            IDirect3DDevice9_SetRenderState(device, D3DRS_ZENABLE, D3DZB_FALSE);
            IDirect3DDevice9_SetRenderState(device, D3DRS_ALPHABLENDENABLE, TRUE);
            IDirect3DDevice9_SetRenderState(device, D3DRS_SRCBLEND, D3DBLEND_SRCALPHA);
            IDirect3DDevice9_SetRenderState(device, D3DRS_DESTBLEND, D3DBLEND_INVSRCALPHA);
            IDirect3DDevice9_SetVertexShader(device, NULL);
            IDirect3DDevice9_SetPixelShader(device, NULL);
            IDirect3DDevice9_SetTexture(device, 0, NULL);
            IDirect3DDevice9_SetFVF(device, D3DFVF_XYZRHW | D3DFVF_DIFFUSE);
            IDirect3DDevice9_SetTextureStageState(device, 0, D3DTSS_COLOROP, D3DTOP_SELECTARG1);
            IDirect3DDevice9_SetTextureStageState(device, 0, D3DTSS_COLORARG1, D3DTA_DIFFUSE);
            IDirect3DDevice9_SetTextureStageState(device, 0, D3DTSS_ALPHAOP, D3DTOP_SELECTARG1);
            IDirect3DDevice9_SetTextureStageState(device, 0, D3DTSS_ALPHAARG1, D3DTA_DIFFUSE);
            IDirect3DDevice9_SetStreamSource(device, 0, pvb, 0, sizeof(struct particle_vertex));
            IDirect3DDevice9_DrawPrimitive(device, D3DPT_TRIANGLELIST, 0, particles * 2);
        }

        IDirect3DDevice9_EndScene(device);
        t1 = now_ms();
        IDirect3DDevice9_Present(device, NULL, NULL, NULL, NULL);
        t2 = now_ms();

        scene_ms += t1 - t0;
        present_ms += t2 - t1;
        if (t2 - t0 > worst_ms) worst_ms = t2 - t0;
        ++frames;
        if (t2 - last >= 1000.0)
        {
            char title[128];
            double fps = frames * 1000.0 / (t2 - last);
            printf("%.1f fps: frame %.2f ms (scene %.2f ms, %.1f us/draw; present %.2f ms), worst %.1f ms\n",
                    fps, (t2 - last) / frames, scene_ms / frames,
                    cubes ? scene_ms / frames * 1000.0 / (cubes + (particles ? 1 : 0)) : 0.0,
                    present_ms / frames, worst_ms);
            fflush(stdout);
            sprintf(title, "Direct3D 9 benchmark - %.1f fps", fps);
            SetWindowTextA(hwnd, title);
            total_frames += frames;
            total_scene += scene_ms;
            total_present += present_ms;
            frames = 0;
            scene_ms = present_ms = worst_ms = 0;
            last = t2;
        }
        if (seconds > 0 && t2 - start >= seconds * 1000.0)
            break;
    }

    total_frames += frames;
    total_scene += scene_ms;
    total_present += present_ms;
    if (total_frames)
        printf("summary: %ld frames in %.1f s: %.1f fps, scene %.2f ms, present %.2f ms\n",
                total_frames, (now_ms() - start) / 1000.0, total_frames * 1000.0 / (now_ms() - start),
                total_scene / total_frames, total_present / total_frames);
    fflush(stdout);

    if (pvb) IDirect3DVertexBuffer9_Release(pvb);
    for (i = 0; i < materials; ++i)
        IDirect3DTexture9_Release(tex[i]);
    IDirect3DIndexBuffer9_Release(ib);
    IDirect3DVertexBuffer9_Release(vb);
    IDirect3DPixelShader9_Release(ps);
    IDirect3DVertexShader9_Release(vs);
    IDirect3DVertexDeclaration9_Release(decl);
    IDirect3DDevice9_Release(device);
    IDirect3D9_Release(d3d);
    return 0;
}
