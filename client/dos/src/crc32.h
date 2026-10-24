/* SPDX-License-Identifier: MIT */
/* CRC-32 (IEEE 802.3), as FILE_COMMIT uses (docs/protocol-v1.md). */
#ifndef SPROCKET_CRC32_H
#define SPROCKET_CRC32_H

#include <stdint.h>

#define CRC32_INIT 0xFFFFFFFFUL

/* Start with CRC32_INIT; the result is the complement of the last value. */
uint32_t crc32_update(uint32_t crc, const uint8_t *data, uint16_t len);

#endif
