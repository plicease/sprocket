/* SPDX-License-Identifier: MIT */
/*
 * SPROCKET: the native protocol on DOS.
 *
 * SPROCKET SERVER [port [baud [dir]]] [/W] answers the host over a serial
 * port until the host closes the session or the user presses Esc. With a
 * directory it serves that tree (read-only unless /W); without one it
 * answers HELLO, PING and CLOSE only.
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
#include "fsys.h"
#include "proto.h"
#include "tree.h"

#define VERSION "0.1.0"
#define MAX_PAYLOAD 1024

/*
 * Check the keyboard and timers once per this many idle polls. Nothing
 * slower than a UART poll and the decoder runs per received byte: on a
 * 4.77 MHz 8088 that is about 330 us, against 1041 us per byte at 9600.
 */
#define IDLE_CHECK 256

static uint8_t rx_buf[MAX_PAYLOAD + FRAME_HEADER_LEN + FRAME_CRC_LEN];
static uint8_t resp_buf[MAX_PAYLOAD + FRAME_HEADER_LEN];

static void send_byte(uint8_t byte)
{
    port_put(byte);
}

static void usage(void)
{
    printf("SPROCKET %s: native file transfer over a serial line.\n\n", VERSION);
    printf("  SPROCKET SERVER [port [baud [dir]]] [/W]\n");
    printf("      Answer the host on port (default COM1 9600). With dir, serve\n");
    printf("      its files; /W also lets the host write them.\n");
}

/* arg is option, ignoring ASCII case; option is in upper case. */
static int is_option(const char *arg, const char *option)
{
    for (; *option; arg++, option++) {
        if ((*arg >= 'a' && *arg <= 'z' ? *arg - ('a' - 'A') : *arg) != *option) {
            return 0;
        }
    }
    return *arg == '\0';
}

static int server(int argc, char **argv)
{
    const char *args[3] = { "COM1", "9600", NULL };
    const char *name;
    long baud;
    const char *dir;
    int writable = 0;
    int count = 0;
    int i;
    struct fs_entry root;
    struct decoder dec;
    struct client cl;
    uint16_t idle = 0;
    uint16_t inter_byte;
    uint32_t last_byte = 0;
    uint32_t now;
    uint8_t received = 0;
    uint32_t frames = 0;
    uint32_t dropped = 0;
    uint8_t byte;
    int result;
    const uint8_t *out;
    uint16_t out_len;

    for (i = 0; i < argc; i++) {
        if (is_option(argv[i], "/W")) {
            writable = 1;
        } else if (argv[i][0] == '/' && argv[i][1] != '\0' && argv[i][2] == '\0') {
            usage();
            return 1;
        } else if (count < 3) {
            args[count++] = argv[i];
        } else {
            usage();
            return 1;
        }
    }
    name = args[0];
    baud = atol(args[1]);
    dir = args[2];
    if (writable && dir == NULL) {
        printf("/W needs a directory to serve.\n");
        return 1;
    }
    if (dir != NULL && (fs_stat(dir, &root) != FS_OK || !root.dir)) {
        printf("Not a directory: %s\n", dir);
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
    if (dir != NULL) {
        tree_init(dir, writable);
        cl.minor = 1;
        cl.capabilities = CAP_FILE_TREE;
        if (writable) {
            cl.capabilities |= CAP_FILE_WRITE | CAP_FILE_REPLACE;
        }
        cl.handler = tree_request;
        cl.reset = tree_reset;
    }

    printf("SPROCKET %s, protocol %d.%d, on %s at %ld 8N1.\n", VERSION,
           PROTO_MAJOR, cl.minor, name, baud);
    if (dir != NULL) {
        printf("Serving %s%s.\n", dir, writable ? ", writable" : ", read-only");
    }
    printf("Waiting for the host; press Esc to exit.\n");

    for (;;) {
        if (port_get(&byte)) {
            idle = 0;
            received = 1;
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
        /* The time of the last byte, to within one idle check. */
        now = port_ticks();
        if (received) {
            received = 0;
            last_byte = now;
        } else if (decoder_in_frame(&dec) && now - last_byte > inter_byte) {
            if (decoder_timeout(&dec) != FRAME_NONE) {
                dropped++;
            }
        }
        if (port_escape()) {
            printf("Cancelled.\n");
            break;
        }
    }

    if (dir != NULL) {
        tree_reset();
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

int main(int argc, char **argv)
{
    if (argc > 1 && is_option(argv[1], "SERVER")) {
        return server(argc - 2, argv + 2);
    }
    usage();
    return 1;
}
