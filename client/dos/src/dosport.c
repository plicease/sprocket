/* SPDX-License-Identifier: MIT */
/*
 * DOS platform layer: an 8250/16450/16550 UART driven by polling, with
 * interrupts off at the UART, so no FIFO and no interrupt handler are
 * needed. This works because the host sends one request at a time and
 * waits: while the client is busy, the line is silent.
 */
#include <dos.h>
#include <io.h>
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

/*
 * Port I/O, inline: a call per UART access would slow the receive path,
 * which polls the line status for every byte. libc16 has no inp or outp.
 */
static unsigned char in_byte(unsigned port);
#pragma aux in_byte = \
    "in al, dx" \
    parm [dx] value [al] modify exact [al];

static void out_byte(unsigned port, unsigned char value);
#pragma aux out_byte = \
    "out dx, al" \
    parm [dx] [al] modify exact [];

/*
 * The BIOS keyboard (INT 16h). Function 01h clears ZF when a key is
 * waiting; LAHF copies the flags to AH, where ZF is bit 6. Function 00h
 * returns the next key, scan code in AH and character in AL.
 */
#define ZF 0x40

static unsigned char key_flags(void);
#pragma aux key_flags = \
    "mov ah, 1" \
    "int 16h" \
    "lahf" \
    "mov al, ah" \
    value [al] modify exact [ax];

static unsigned key_read(void);
#pragma aux key_read = \
    "xor ah, ah" \
    "int 16h" \
    value [ax] modify exact [ax];

#define key_ready() (!(key_flags() & ZF))

static unsigned base;
static uint16_t errors;

const char port_default[] = "COM1";

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

    out_byte(base + IER, 0);
    out_byte(base + LCR, LCR_DLAB);
    out_byte(base + DLL, divisor & 0xFF);
    out_byte(base + DLM, divisor >> 8);
    out_byte(base + LCR, LCR_8N1);
    /* Leave any FIFO off: the protocol is designed not to need one. */
    out_byte(base + FCR, 0);
    out_byte(base + MCR, MCR_DTR_RTS);
    /* Discard anything already received. */
    while (in_byte(base + LSR) & LSR_DATA) {
        (void)in_byte(base + RBR);
    }
    errors = 0;
    return 0;
}

int port_get(uint8_t *byte)
{
    unsigned lsr = in_byte(base + LSR);

    if (lsr & LSR_ERRORS) {
        errors++;
    }
    if (!(lsr & LSR_DATA)) {
        return 0;
    }
    *byte = (uint8_t)in_byte(base + RBR);
    return 1;
}

void port_put(uint8_t byte)
{
    while (!(in_byte(base + LSR) & LSR_THRE)) {
    }
    out_byte(base + THR, byte);
}

void port_close(void)
{
    while (!(in_byte(base + LSR) & LSR_TEMT)) {
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
    if (!key_ready()) {
        return 0;
    }
    return (key_read() & 0xFF) == 0x1B;
}

int port_key(void)
{
    unsigned key;
    unsigned char c;

    /* Commands from a file or pipe (SPROCKET CLIENT < CMDS.TXT). */
    if (!isatty(0)) {
        return read(0, &c, 1) == 1 ? c : PORT_EOF;
    }
    if (!key_ready()) {
        return PORT_NO_KEY;
    }
    key = key_read() & 0xFF;
    /* 0 and E0 come with keys that have no ASCII code, such as arrows. */
    return key == 0 || key == 0xE0 ? PORT_NO_KEY : (int)key;
}
