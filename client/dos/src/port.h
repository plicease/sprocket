/* SPDX-License-Identifier: MIT */
/*
 * The platform layer: serial port, clock and keyboard. dosport.c drives an
 * 8250-class UART by polling; posixport.c uses a Linux tty or PTY so the
 * client can be tested on the host.
 */
#ifndef SPROCKET_PORT_H
#define SPROCKET_PORT_H

#include <stdint.h>

/* Open the named port at baud, 8N1, no flow control. Returns 0 on success,
 * or prints an error and returns -1. */
int port_open(const char *name, long baud);

/* Fetch one received byte if there is one. Returns 1 if *byte was set. */
int port_get(uint8_t *byte);

/* Send one byte, waiting for room in the transmitter. */
void port_put(uint8_t byte);

/* Wait until everything sent has left the port, then release it. */
void port_close(void);

/* Receive errors (overruns, framing) seen by the port, if it can tell. */
uint16_t port_errors(void);

/* A free-running clock, and its rate. */
uint32_t port_ticks(void);
uint16_t port_ticks_per_second(void);

/* Nonzero if the user pressed Esc. */
int port_escape(void);

#endif
