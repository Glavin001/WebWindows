/* Sound through winmm's waveOut and through DirectSound, both of which play
 * through mmdevapi and the audio driver. Checks what a program can observe:
 * a buffer completes after about its length, the play position moves, and
 * a DirectSound buffer's play cursor advances.
 *
 * libs: -lwinmm -ldsound -ldxguid
 * native: skip (CI's Windows machines have no sound device)
 */
#include <windows.h>
#include <mmsystem.h>
#include <dsound.h>
#include <math.h>
#include <stdio.h>

#define RATE 22050

static void tone(short *s, int n, int channels, double hz)
{
    int i, c;
    for (i = 0; i < n; i++)
        for (c = 0; c < channels; c++)
            s[i * channels + c] = (short)(8000 * sin(2 * 3.14159265358979 * hz * i / RATE));
}

static void wave_out(void)
{
    WAVEFORMATEX fmt = { WAVE_FORMAT_PCM, 1, RATE, RATE * 2, 2, 16, 0 };
    static short samples[RATE / 4];
    WAVEHDR hdr = { 0 };
    HWAVEOUT out;
    MMTIME pos = { TIME_SAMPLES };
    HANDLE done = CreateEventA(NULL, FALSE, FALSE, NULL);
    DWORD t0, elapsed;
    MMRESULT r;

    tone(samples, RATE / 4, 1, 440);
    r = waveOutOpen(&out, WAVE_MAPPER, &fmt, (DWORD_PTR)done, 0, CALLBACK_EVENT);
    printf("waveOutOpen: %u\n", r);
    if (r) return;
    WaitForSingleObject(done, 1000); /* WOM_OPEN */
    hdr.lpData = (char *)samples;
    hdr.dwBufferLength = sizeof(samples);
    waveOutPrepareHeader(out, &hdr, sizeof(hdr));
    t0 = GetTickCount();
    printf("waveOutWrite: %u\n", waveOutWrite(out, &hdr, sizeof(hdr)));
    while (!(hdr.dwFlags & WHDR_DONE) && GetTickCount() - t0 < 3000) WaitForSingleObject(done, 100);
    elapsed = GetTickCount() - t0;
    printf("buffer done: %d, after about its length (250 ms): %d\n", !!(hdr.dwFlags & WHDR_DONE),
           elapsed >= 200 && elapsed < 1500);
    waveOutGetPosition(out, &pos, sizeof(pos));
    printf("position in samples reached the buffer's end: %d\n", pos.u.sample >= RATE / 4 - 64);
    waveOutUnprepareHeader(out, &hdr, sizeof(hdr));
    printf("waveOutClose: %u\n", waveOutClose(out));
    CloseHandle(done);
}

static void direct_sound(void)
{
    WAVEFORMATEX fmt = { WAVE_FORMAT_PCM, 2, RATE, RATE * 4, 4, 16, 0 };
    DSBUFFERDESC desc = { sizeof(desc) };
    IDirectSound *ds;
    IDirectSoundBuffer *buf;
    void *p1, *p2;
    DWORD n1, n2, play = 0, write = 0, status = 0;
    HRESULT hr;

    hr = DirectSoundCreate(NULL, &ds, NULL);
    printf("DirectSoundCreate: %08lx\n", hr);
    if (FAILED(hr)) return;
    printf("SetCooperativeLevel: %08lx\n", IDirectSound_SetCooperativeLevel(ds, GetDesktopWindow(), DSSCL_PRIORITY));
    desc.dwFlags = DSBCAPS_GETCURRENTPOSITION2 | DSBCAPS_GLOBALFOCUS;
    desc.dwBufferBytes = RATE * 4; /* one second */
    desc.lpwfxFormat = &fmt;
    hr = IDirectSound_CreateSoundBuffer(ds, &desc, &buf, NULL);
    printf("CreateSoundBuffer: %08lx\n", hr);
    if (FAILED(hr)) return;
    hr = IDirectSoundBuffer_Lock(buf, 0, desc.dwBufferBytes, &p1, &n1, &p2, &n2, 0);
    printf("Lock: %08lx, %lu bytes\n", hr, n1 + n2);
    tone(p1, n1 / 4, 2, 660);
    IDirectSoundBuffer_Unlock(buf, p1, n1, p2, n2);
    printf("Play: %08lx\n", IDirectSoundBuffer_Play(buf, 0, 0, 0));
    IDirectSoundBuffer_GetStatus(buf, &status);
    printf("playing: %d\n", !!(status & DSBSTATUS_PLAYING));
    Sleep(300);
    IDirectSoundBuffer_GetCurrentPosition(buf, &play, &write);
    printf("play cursor moved: %d, within the first second: %d\n", play > 0, play < desc.dwBufferBytes);
    printf("Stop: %08lx\n", IDirectSoundBuffer_Stop(buf));
    IDirectSoundBuffer_GetStatus(buf, &status);
    printf("stopped: %d\n", !(status & DSBSTATUS_PLAYING));
    IDirectSoundBuffer_Release(buf);
    IDirectSound_Release(ds);
}

int main(void)
{
    setvbuf(stdout, NULL, _IONBF, 0);
    printf("waveOut devices: %u\n", waveOutGetNumDevs());
    wave_out();
    direct_sound();
    printf("done\n");
    return 0;
}
