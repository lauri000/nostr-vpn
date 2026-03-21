#pragma once

#include <stdbool.h>
#include <stdint.h>

typedef void (*nvpn_ios_settings_callback_t)(const char *json, uintptr_t context);
typedef void (*nvpn_ios_packet_callback_t)(const uint8_t *packet, uintptr_t length, uintptr_t context);

bool nvpn_ios_extension_start(
    const char *config_json,
    uintptr_t context,
    nvpn_ios_settings_callback_t settings_callback,
    nvpn_ios_packet_callback_t packet_callback
);

void nvpn_ios_extension_push_packet(const uint8_t *packet, uintptr_t length);
void nvpn_ios_extension_stop(void);
char *nvpn_ios_extension_status_json(void);
void nvpn_ios_extension_free_string(char *value);
