/* SPDX-License-Identifier: MIT */
/*
 * The exact-width integer types, for the DOS build: libc16 has no
 * <stdint.h>. Open Watcom's 16-bit targets have an 8-bit char, a 16-bit
 * short and int, a 32-bit long and a 64-bit long long.
 */
#ifndef SPROCKET_STDINT_H
#define SPROCKET_STDINT_H

typedef signed char int8_t;
typedef unsigned char uint8_t;
typedef short int16_t;
typedef unsigned short uint16_t;
typedef long int32_t;
typedef unsigned long uint32_t;
typedef long long int64_t;
typedef unsigned long long uint64_t;

#endif
