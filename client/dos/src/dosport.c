/* SPDX-License-Identifier: MIT */
/*
 * DOS platform layer: an 8250/16450/16550 UART driven by polling, with
 * interrupts off at the UART, so no FIFO and no interrupt handler are
 * needed. This works because the host sends one request at a time and
 * waits: while the client is busy, the line is silent.
 */
#include <bios.h>
#include <conio.h>
#include <i86.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "port.h"

/* UART registers, as offsets from the base port. */
#define RBR 0 /* receive buffer (read) */
#define THR 0 /* transmit holding (write) */
#define IER 1 /* interrupt enable */
#define FCR 2 /* FIFO control (16550; ignored by older parts) */
#define LCR 3 /* line control */
#define MCR 4 /* modem control */
#define LSR 5 /* line status */
#define DLL 0 /* divisor latch, low (with LCR bit 7 set) */
#define DLM 1 /* divisor latch, high */

#define LCR_8N1 0x03
#define LCR_DLAB 0x80
#define MCR_DTR_RTS 0x03
#define LSR_DATA 0x01
#define LSR_ERRORS 0x0E /* overrun, parity, framing */
#define LSR_THRE 0x20   /* holding register empty */
#define LSR_TEMT 0x40   /* transmitter completely empty */

#define UART_CLOCK 115200L

static unsigned base;
static uint16_t errors;

int port_open(const char *name, long baud)
{
    unsigned short __far *bios_ports = (unsigned short __far *)MK_FP(0x40, 0);
    int n;
    unsigned divisor;

    if (strlen(name) != 4 || strnicmp(name, "COM", 3) != 0 || name[3] < '1'
        || name[3] > '4') {
        printf("Unknown port %s; use COM1 to COM4.\n", name);
        return -1;
    }
    n = name[3] - '1';
    /* The BIOS records the base address of each port it found. */
    base = bios_ports[n];
    if (base == 0) {
        printf("%s is not installed.\n", name);
        return -1;
    }
    if (baud < 50 || baud > UART_CLOCK || UART_CLOCK % baud != 0) {
        printf("Unsupported baud rate %ld.\n", baud);
        return -1;
    }
    divisor = (unsigned)(UART_CLOCK / baud);

    outp(base + IER, 0);
    outp(base + LCR, LCR_DLAB);
    outp(base + DLL, divisor & 0xFF);
    outp(base + DLM, divisor >> 8);
    outp(base + LCR, LCR_8N1);
    /* Leave any FIFO off: the protocol is designed not to need one. */
    outp(base + FCR, 0);
    outp(base + MCR, MCR_DTR_RTS);
    /* Discard anything already received. */
    while (inp(base + LSR) & LSR_DATA) {
        (void)inp(base + RBR);
    }
    errors = 0;
    return 0;
}

int port_get(uint8_t *byte)
{
    unsigned lsr = inp(base + LSR);

    if (lsr & LSR_ERRORS) {
        errors++;
    }
    if (!(lsr & LSR_DATA)) {
        return 0;
    }
    *byte = (uint8_t)inp(base + RBR);
    return 1;
}

void port_put(uint8_t byte)
{
    while (!(inp(base + LSR) & LSR_THRE)) {
    }
    outp(base + THR, byte);
}

void port_close(void)
{
    while (!(inp(base + LSR) & LSR_TEMT)) {
    }
}

uint16_t port_errors(void)
{
    return errors;
}

uint32_t port_ticks(void)
{
    /* The BIOS timer tick count at 0040:006C, about 18.2 per second. */
    volatile uint32_t __far *ticks = (volatile uint32_t __far *)MK_FP(0x40, 0x6C);
    uint32_t a;
    uint32_t b;

    /* The timer interrupt may update it between the two word reads. */
    do {
        a = *ticks;
        b = *ticks;
    } while (a != b);
    return a;
}

uint16_t port_ticks_per_second(void)
{
    return 18;
}

int port_escape(void)
{
    if (!_bios_keybrd(_KEYBRD_READY)) {
        return 0;
    }
    return (_bios_keybrd(_KEYBRD_READ) & 0xFF) == 0x1B;
}
