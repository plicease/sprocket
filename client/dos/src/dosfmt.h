/* SPDX-License-Identifier: MIT */
/*
 * printf and sprintf for the DOS build (dosfmt.c). libc16, the DOS
 * runtime, has no printf family, so the Makefile includes this header in
 * every DOS object (wcc -fi) and the shared sources call printf as usual.
 */
#ifndef SPROCKET_DOSFMT_H
#define SPROCKET_DOSFMT_H

int printf(const char *format, ...);
int sprintf(char *buf, const char *format, ...);

#endif
