/* SPDX-License-Identifier: MIT */
#include "crc32.h"

/*
 * A nibble at a time, with a 64-byte table instead of 1 KB: files move at
 * serial speed, so the CRC is never the bottleneck.
 */
static const uint32_t crc_nibble[16] = {
    0x00000000UL, 0x1DB71064UL, 0x3B6E20C8UL, 0x26D930ACUL,
    0x76DC4190UL, 0x6B6B51F4UL, 0x4DB26158UL, 0x5005713CUL,
    0xEDB88320UL, 0xF00F9344UL, 0xD6D6A3E8UL, 0xCB61B38CUL,
    0x9B64C2B0UL, 0x86D3D2D4UL, 0xA00AE278UL, 0xBDBDF21CUL
};

uint32_t crc32_update(uint32_t crc, const uint8_t *data, uint16_t len)
{
    while (len--) {
        crc ^= *data++;
        crc = (crc >> 4) ^ crc_nibble[crc & 0x0F];
        crc = (crc >> 4) ^ crc_nibble[crc & 0x0F];
    }
    return crc;
}
