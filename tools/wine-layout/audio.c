/* Prints the 32-bit layouts of mmdevapi's audio driver calls (the structs
 * in dlls/mmdevapi/unixlib.h), which the host's browser audio driver
 * reads and writes (runtime/wine/audio.mjs):
 *
 *   tools/wine-layout/gen.sh audio > runtime/wine/audio-layout.json
 */
#include <stdarg.h>
#include <stddef.h>
#define COBJMACROS
#include <windef.h>
#include <winbase.h>
#include <wingdi.h>
#include <winternl.h>
#include <mmreg.h>
#include <audioclient.h>
#include <mmdeviceapi.h>
#include "unixlib.h"

static void out(const char *s) {
    DWORD n = 0, w;
    while (s[n]) n++;
    WriteFile(GetStdHandle(STD_OUTPUT_HANDLE), s, n, &w, NULL);
}
static void num(unsigned v) {
    char b[12];
    int i = 11;
    b[i] = 0;
    do { b[--i] = '0' + v % 10; v /= 10; } while (v);
    out(b + i);
}
#define F(s, f) (out("    \"" #f "\": "), num((unsigned)offsetof(struct s, f)), out(",\n"))
#define BEGIN(s) (out("  \"" #s "\": {\n    \"__size\": "), num((unsigned)sizeof(struct s)), out(",\n"))
#define END() out("    \"__end\": 0\n  },\n")

void entry(void) {
    out("{\n");
    BEGIN(get_endpoint_ids_params); F(get_endpoint_ids_params, flow); F(get_endpoint_ids_params, endpoints);
    F(get_endpoint_ids_params, size); F(get_endpoint_ids_params, result); F(get_endpoint_ids_params, num);
    F(get_endpoint_ids_params, default_idx); END();
    BEGIN(create_stream_params); F(create_stream_params, name); F(create_stream_params, device);
    F(create_stream_params, flow); F(create_stream_params, share); F(create_stream_params, flags);
    F(create_stream_params, duration); F(create_stream_params, period); F(create_stream_params, fmt);
    F(create_stream_params, result); F(create_stream_params, channel_count); F(create_stream_params, stream); END();
    BEGIN(release_stream_params); F(release_stream_params, stream); F(release_stream_params, timer_thread);
    F(release_stream_params, result); END();
    BEGIN(start_params); F(start_params, stream); F(start_params, result); END();
    BEGIN(timer_loop_params); F(timer_loop_params, stream); END();
    BEGIN(get_render_buffer_params); F(get_render_buffer_params, stream); F(get_render_buffer_params, frames);
    F(get_render_buffer_params, result); F(get_render_buffer_params, data); END();
    BEGIN(release_render_buffer_params); F(release_render_buffer_params, stream);
    F(release_render_buffer_params, written_frames); F(release_render_buffer_params, flags);
    F(release_render_buffer_params, result); END();
    BEGIN(is_format_supported_params); F(is_format_supported_params, device); F(is_format_supported_params, flow);
    F(is_format_supported_params, share); F(is_format_supported_params, fmt_in); F(is_format_supported_params, result); END();
    BEGIN(get_mix_format_params); F(get_mix_format_params, device); F(get_mix_format_params, flow);
    F(get_mix_format_params, fmt); F(get_mix_format_params, result); END();
    BEGIN(get_device_period_params); F(get_device_period_params, device); F(get_device_period_params, flow);
    F(get_device_period_params, result); F(get_device_period_params, def_period); F(get_device_period_params, min_period); END();
    BEGIN(get_buffer_size_params); F(get_buffer_size_params, stream); F(get_buffer_size_params, result);
    F(get_buffer_size_params, frames); END();
    BEGIN(get_latency_params); F(get_latency_params, stream); F(get_latency_params, result); F(get_latency_params, latency); END();
    BEGIN(get_current_padding_params); F(get_current_padding_params, stream); F(get_current_padding_params, result);
    F(get_current_padding_params, padding); END();
    BEGIN(get_frequency_params); F(get_frequency_params, stream); F(get_frequency_params, result); F(get_frequency_params, freq); END();
    BEGIN(get_position_params); F(get_position_params, stream); F(get_position_params, device);
    F(get_position_params, result); F(get_position_params, pos); F(get_position_params, qpctime); END();
    BEGIN(set_volumes_params); F(set_volumes_params, stream); F(set_volumes_params, master_volume);
    F(set_volumes_params, volumes); F(set_volumes_params, session_volumes); END();
    BEGIN(set_event_handle_params); F(set_event_handle_params, stream); F(set_event_handle_params, event);
    F(set_event_handle_params, result); END();
    BEGIN(test_connect_params); F(test_connect_params, name); F(test_connect_params, priority); END();
    BEGIN(get_prop_value_params); F(get_prop_value_params, device); F(get_prop_value_params, flow);
    F(get_prop_value_params, guid); F(get_prop_value_params, prop); F(get_prop_value_params, result);
    F(get_prop_value_params, value); F(get_prop_value_params, buffer); F(get_prop_value_params, buffer_size); END();
    BEGIN(midi_init_params); F(midi_init_params, err); END();
    BEGIN(midi_out_message_params); F(midi_out_message_params, err); F(midi_out_message_params, notify); END();
    BEGIN(aux_message_params); F(aux_message_params, err); END();
    out("  \"__end\": 0\n}\n");
    ExitProcess(0);
}
