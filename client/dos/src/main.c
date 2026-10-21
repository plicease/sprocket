/* SPDX-License-Identifier: MIT */
/*
 * SPROCKET client: answers the host over a serial port until the host
 * closes the session or the user presses Esc.
 *
 * Usage: SPROCKET [port [baud]]    default COM1 9600
 *
 * Screen output happens only before a response is sent, never between
 * sending a response and reading the next request: printing scrolls the
 * screen, which on an 8088 is slow enough to overrun a UART without a
 * FIFO, and the host sends its next request as soon as it has a response.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "frame.h"
#include "port.h"
#include "proto.h"

#define VERSION "0.1.0"
#define MAX_PAYLOAD 1024

/* Check the keyboard and timers once per this many idle polls. */
#define IDLE_CHECK 256

static uint8_t rx_buf[MAX_PAYLOAD + FRAME_HEADER_LEN + FRAME_CRC_LEN];
static uint8_t resp_buf[MAX_PAYLOAD + FRAME_HEADER_LEN];

static void send_byte(uint8_t byte)
{
    port_put(byte);
}

int main(int argc, char **argv)
{
    const char *name = argc > 1 ? argv[1] : "COM1";
    long baud = argc > 2 ? atol(argv[2]) : 9600L;
    struct decoder dec;
    struct client cl;
    uint16_t idle = 0;
    uint16_t inter_byte;
    uint32_t last_byte = 0;
    uint32_t frames = 0;
    uint32_t dropped = 0;
    uint8_t byte;
    int result;
    const uint8_t *out;
    uint16_t out_len;

    if (argc > 3 || (argc > 1 && strcmp(argv[1], "/?") == 0)) {
        printf("Usage: SPROCKET [port [baud]]   (default COM1 9600)\n");
        return 1;
    }
    if (port_open(name, baud) != 0) {
        return 1;
    }
    inter_byte = port_ticks_per_second();

    decoder_init(&dec, rx_buf, MAX_PAYLOAD);
    cl.max_payload = MAX_PAYLOAD;
    cl.capabilities = 0;
    cl.platform = PLATFORM_DOS;
    cl.version = VERSION;
    client_init(&cl, resp_buf);

    printf("SPROCKET %s, protocol %d.%d, on %s at %ld 8N1.\n", VERSION,
           PROTO_MAJOR, PROTO_MINOR, name, baud);
    printf("Waiting for the host; press Esc to exit.\n");

    for (;;) {
        if (port_get(&byte)) {
            idle = 0;
            last_byte = port_ticks();
            result = decoder_push(&dec, byte);
            if (result == FRAME_OK) {
                frames++;
                if (client_handle(&cl, dec.buf, dec.frame_len, dec.crc, &out,
                                  &out_len)) {
                    if (cl.started) {
                        cl.started = 0;
                        printf("Session %04X started.\n", cl.session);
                    }
                    frame_send(out, out_len, send_byte);
                    if (cl.closed) {
                        break;
                    }
                }
            } else if (result != FRAME_NONE) {
                dropped++;
            }
            continue;
        }
        if (++idle < IDLE_CHECK) {
            continue;
        }
        idle = 0;
        if (decoder_in_frame(&dec)
            && port_ticks() - last_byte > inter_byte) {
            if (decoder_timeout(&dec) != FRAME_NONE) {
                dropped++;
            }
        }
        if (port_escape()) {
            printf("Cancelled.\n");
            break;
        }
    }

    port_close();
    if (cl.closed) {
        printf("Session %04X closed by the host.\n", cl.session);
    }
    printf("%lu frames, %lu dropped, %u repeated, %u receive errors.\n",
           (unsigned long)frames, (unsigned long)dropped, cl.duplicates,
           port_errors());
    return 0;
}
