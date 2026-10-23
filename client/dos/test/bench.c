/* SPDX-License-Identifier: MIT */
/*
 * BENCH.EXE: times the client's receive path on the machine it runs on, to
 * show whether polling keeps up with a UART that has no FIFO.
 *
 * Per received byte, the main loop polls the UART and feeds the decoder.
 * This repeats that work over encoded
 * 1024-byte PING frames (port_get still reads the real UART's status
 * register) and reports the time per byte against the time one byte takes
 * on the wire. It also times the end of a frame (CRC check and building the
 * response), during which the host is waiting and the line is idle.
 *
 * The BIOS timer ticks only 18.2 times a second, so each test repeats
 * frames until at least MIN_TICKS have passed (checking the timer only
 * between frames, never per byte); a fast machine runs many more frames.
 *
 * Usage: BENCH [port]    default COM1
 */
#include <stdio.h>

#include "../src/frame.h"
#include "../src/port.h"
#include "../src/proto.h"

#define PAYLOAD 1024
#define FRAMES 20
/* About two seconds of BIOS ticks: under 3% rounding error per test. */
#define MIN_TICKS 36
/* One BIOS tick is 54925 microseconds. */
#define TICK_US 54925UL

static uint8_t wire[2 * (PAYLOAD + FRAME_HEADER_LEN + FRAME_CRC_LEN) + 2];
static uint16_t wire_len;
static uint8_t rx_buf[PAYLOAD + FRAME_HEADER_LEN + FRAME_CRC_LEN];
static uint8_t resp_buf[PAYLOAD + FRAME_HEADER_LEN];
static uint8_t body[PAYLOAD + FRAME_HEADER_LEN];

static void collect(uint8_t byte)
{
    wire[wire_len++] = byte;
}

static void discard(uint8_t byte)
{
    (void)byte;
}

/* Wait for the next tick so timing starts on a boundary. */
static uint32_t tick_edge(void)
{
    uint32_t t = port_ticks();
    while (port_ticks() == t) {
    }
    return port_ticks();
}

int main(int argc, char **argv)
{
    static const long rates[] = { 9600L, 19200L, 38400L, 57600L, 115200L };
    struct decoder dec;
    struct client cl;
    uint32_t start;
    uint32_t ticks;
    uint32_t bytes = 0;
    uint16_t i;
    uint32_t f;
    uint8_t dummy;
    unsigned long frames = 0;
    unsigned long tenths_per_byte;
    unsigned long us_end;
    const uint8_t *out;
    uint16_t out_len;

    if (port_open(argc > 1 ? argv[1] : "COM1", 9600L) != 0) {
        return 1;
    }

    /* A PING with every byte value, as `sprocket hello` sends. */
    body[0] = MSG_PING;
    body[1] = 0x3C;
    body[2] = 0x5A;
    body[3] = 1;
    for (i = 0; i < PAYLOAD; i++) {
        body[FRAME_HEADER_LEN + i] = (uint8_t)i;
    }
    frame_send(body, sizeof body, collect);
    decoder_init(&dec, rx_buf, PAYLOAD);

    printf("Timing the receive path over frames of %u bytes...\n", wire_len);
    start = tick_edge();
    for (f = 0; f < FRAMES || port_ticks() - start < MIN_TICKS; f++) {
        for (i = 0; i < wire_len; i++) {
            (void)port_get(&dummy);
            if (decoder_push(&dec, wire[i]) == FRAME_OK) {
                frames++;
            }
            bytes++;
        }
    }
    ticks = port_ticks() - start;
    /* In tenths of a microsecond, so fast machines still show a figure. */
    tenths_per_byte = ticks * TICK_US * 10 / bytes;
    printf("%lu bytes in %lu ticks: about %lu.%lu us per byte (%lu frames decoded)\n",
           (unsigned long)bytes, (unsigned long)ticks, tenths_per_byte / 10,
           tenths_per_byte % 10, frames);

    cl.max_payload = PAYLOAD;
    cl.capabilities = 0;
    cl.platform = PLATFORM_DOS;
    cl.version = "bench";
    client_init(&cl, resp_buf);
    start = tick_edge();
    for (f = 0; f < FRAMES || port_ticks() - start < MIN_TICKS; f++) {
        /* The CRC check at the end of a frame, then the response. */
        (void)crc16(body, sizeof body);
        body[3] = (uint8_t)f;
        (void)client_handle(&cl, body, sizeof body, (uint16_t)f, &out, &out_len);
        frame_send(out, out_len, discard);
    }
    ticks = port_ticks() - start;
    us_end = ticks * TICK_US / f;
    printf("End of frame (CRC, handle, encode response): about %lu.%lu ms\n",
           us_end / 1000, us_end / 100 % 10);

    printf("\n  baud   us/byte on the wire   margin\n");
    for (i = 0; i < sizeof rates / sizeof rates[0]; i++) {
        unsigned long wire_us = 10000000UL / (unsigned long)rates[i];
        unsigned long margin;

        if (tenths_per_byte == 0) {
            printf("%7ld   %8lu              keeps up, over %lux\n", rates[i],
                   wire_us, wire_us * 10);
            continue;
        }
        /* In tenths: wire time over receive time. */
        margin = wire_us * 100 / tenths_per_byte;
        printf("%7ld   %8lu              %s %lu.%lux\n", rates[i], wire_us,
               margin >= 10 ? "keeps up," : "TOO SLOW,", margin / 10,
               margin % 10);
    }
    port_close();
    return 0;
}
