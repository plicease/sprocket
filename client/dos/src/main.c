/* SPDX-License-Identifier: MIT */
/*
 * SPROCKET: the native protocol on DOS.
 *
 * SPROCKET SERVER [port [baud [dir]]] [/W] [/D] [/1] answers clients over
 * a serial port, one session after another, until the user presses Esc
 * (or, with /1, until the first client closes its session). With a
 * directory it serves that tree (read-only unless /W); with /D it offers
 * the BIOS disks for imaging (storage.c); with neither it answers HELLO,
 * PING and CLOSE only.
 *
 * SPROCKET CLIENT [port [baud]] calls a server and runs the file-transfer
 * shell (cli.c).
 *
 * The port defaults to port_default (COM1 on DOS), and the baud rate to
 * SPROCKET_BAUD from the environment, or 9600 if that is not set.
 *
 * Screen output happens only before a response is sent, never between
 * sending a response and reading the next request: printing scrolls the
 * screen, which on an 8088 is slow enough to overrun a UART without a
 * FIFO, and the host sends its next request as soon as it has a response.
 */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include "cli.h"
#include "frame.h"
#include "port.h"
#include "fsys.h"
#include "proto.h"
#include "storage.h"
#include "tree.h"

#define VERSION "0.3.0"
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
    printf("  SPROCKET CLIENT [port [baud]]\n");
    printf("      Call a server on port and copy files to and from it.\n");
    printf("  SPROCKET SERVER [port [baud [dir]]] [/W] [/D] [/1]\n");
    printf("      Answer clients on port until Esc is pressed. With dir, serve\n");
    printf("      its files; /W also lets clients write them. /D offers the\n");
    printf("      disks to image (read-only). /1 exits when the first client\n");
    printf("      closes its session.\n\n");
    printf("The port defaults to %s, and baud to SPROCKET_BAUD if set, else 9600.\n",
           port_default);
}

/* The baud rate when none is given: SPROCKET_BAUD, or 9600. */
static long default_baud(void)
{
    const char *env = getenv("SPROCKET_BAUD");

    return env != NULL && *env != '\0' ? atol(env) : 9600L;
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

static int serve_disks;
static int serve_tree;

/* Requests beyond HELLO, PING and CLOSE: the disks and the file tree,
 * each only when it is being served. */
static int server_request(uint8_t kind, const uint8_t *in, uint16_t in_len, uint8_t *out,
                          uint16_t limit, uint8_t *class, uint16_t *code)
{
    if (kind == MSG_STORAGE_LIST || kind == MSG_STORAGE_READ) {
        if (serve_disks) {
            return storage_request(kind, in, in_len, out, limit, class, code);
        }
    } else if (serve_tree) {
        return tree_request(kind, in, in_len, out, limit, class, code);
    }
    *code = 0;
    *class = ERR_UNSUPPORTED;
    return -1;
}

static int server(int argc, char **argv)
{
    const char *args[3] = { NULL, NULL, NULL };
    const char *name;
    long baud;
    const char *dir;
    int writable = 0;
    int once = 0;
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
        } else if (is_option(argv[i], "/1")) {
            once = 1;
        } else if (is_option(argv[i], "/D")) {
            serve_disks = 1;
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
    name = args[0] != NULL ? args[0] : port_default;
    baud = args[1] != NULL ? atol(args[1]) : default_baud();
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
        serve_tree = 1;
        cl.minor = 1;
        cl.capabilities = CAP_FILE_TREE;
        if (writable) {
            cl.capabilities |= CAP_FILE_WRITE | CAP_FILE_REPLACE;
        }
        cl.handler = server_request;
        cl.reset = tree_reset;
    }
    if (serve_disks) {
        cl.capabilities |= STORAGE_CAPS;
        cl.handler = server_request;
    }

    printf("SPROCKET %s, protocol %d.%d, on %s at %ld 8N1.\n", VERSION,
           PROTO_MAJOR, cl.minor, name, baud);
    if (dir != NULL) {
        printf("Serving %s%s.\n", dir, writable ? ", writable" : ", read-only");
    }
    if (serve_disks) {
        printf("Offering %u disk(s) to image, read-only.\n", storage_init());
    }
    printf("Waiting for a client; press Esc to exit.\n");

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
                        if (once) {
                            break;
                        }
                        /*
                         * Safe to print after a response here: the host
                         * sends nothing more unless that response was
                         * lost, and then only after its timeout. A resent
                         * CLOSE is answered from the cache.
                         */
                        cl.closed = 0;
                        printf("Session %04X closed by the client.\n", cl.session);
                        printf("Waiting for a client; press Esc to exit.\n");
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
        printf("Session %04X closed by the client.\n", cl.session);
    }
    printf("%lu frames, %lu dropped, %u repeated, %u receive errors.\n",
           (unsigned long)frames, (unsigned long)dropped, cl.duplicates,
           port_errors());
    return 0;
}

static int client(int argc, char **argv)
{
    if (argc > 2 || (argc > 0 && argv[0][0] == '/' && argv[0][1] != '\0' && argv[0][2] == '\0')) {
        usage();
        return 1;
    }
    return cli_run(argc > 0 ? argv[0] : port_default, argc > 1 ? atol(argv[1]) : default_baud(),
                   resp_buf, rx_buf, MAX_PAYLOAD, 0);
}

int main(int argc, char **argv)
{
    if (argc > 1 && is_option(argv[1], "SERVER")) {
        return server(argc - 2, argv + 2);
    }
    if (argc > 1 && is_option(argv[1], "CLIENT")) {
        return client(argc - 2, argv + 2);
    }
    usage();
    return 1;
}
