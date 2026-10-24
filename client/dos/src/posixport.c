/* SPDX-License-Identifier: MIT */
/*
 * Host platform layer for testing the program on Linux: a tty or PTY in
 * raw mode. Esc is not read; stop it with Ctrl-C. Keys for the client's
 * prompt come from standard input, which may be a pipe.
 */
#define _DEFAULT_SOURCE
#include <errno.h>
#include <stdlib.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

#include "port.h"

static int fd = -1;
static struct termios saved_stdin;

static speed_t speed(long baud)
{
    switch (baud) {
    case 1200: return B1200;
    case 2400: return B2400;
    case 4800: return B4800;
    case 9600: return B9600;
    case 19200: return B19200;
    case 38400: return B38400;
    case 57600: return B57600;
    case 115200: return B115200;
    default: return 0;
    }
}

const char port_default[] = "/dev/ttyS0";

int port_open(const char *name, long baud)
{
    struct termios t;

    if (speed(baud) == 0) {
        printf("Unsupported baud rate %ld.\n", baud);
        return -1;
    }
    fd = open(name, O_RDWR | O_NOCTTY);
    if (fd < 0 || tcgetattr(fd, &t) != 0) {
        printf("Cannot open %s: %s\n", name, strerror(errno));
        return -1;
    }
    cfmakeraw(&t);
    t.c_cflag |= CLOCAL | CREAD;
    t.c_cflag &= ~(tcflag_t)CRTSCTS;
    cfsetispeed(&t, speed(baud));
    cfsetospeed(&t, speed(baud));
    t.c_cc[VMIN] = 0;
    t.c_cc[VTIME] = 0;
    tcsetattr(fd, TCSANOW, &t);
    tcflush(fd, TCIOFLUSH);
    return 0;
}

int port_get(uint8_t *byte)
{
    struct pollfd p;

    p.fd = fd;
    p.events = POLLIN;
    /* Wait briefly rather than spin. */
    if (poll(&p, 1, 1) <= 0) {
        return 0;
    }
    return read(fd, byte, 1) == 1;
}

void port_put(uint8_t byte)
{
    while (write(fd, &byte, 1) != 1) {
    }
}

void port_close(void)
{
    tcdrain(fd);
    close(fd);
}

uint16_t port_errors(void)
{
    return 0;
}

uint32_t port_ticks(void)
{
    struct timespec ts;

    clock_gettime(CLOCK_MONOTONIC, &ts);
    return (uint32_t)(ts.tv_sec * 1000 + ts.tv_nsec / 1000000);
}

uint16_t port_ticks_per_second(void)
{
    return 1000;
}

int port_escape(void)
{
    return 0;
}

static void restore_stdin(void)
{
    tcsetattr(0, TCSANOW, &saved_stdin);
}

int port_key(void)
{
    static int ready;
    struct termios t;
    struct pollfd p;
    unsigned char c;

    if (!ready) {
        ready = 1;
        /* Keys one at a time, without the terminal's echo; Ctrl-C still
         * raises SIGINT. */
        if (isatty(0) && tcgetattr(0, &saved_stdin) == 0) {
            t = saved_stdin;
            t.c_lflag &= ~(tcflag_t)(ICANON | ECHO);
            t.c_cc[VMIN] = 1;
            t.c_cc[VTIME] = 0;
            tcsetattr(0, TCSANOW, &t);
            atexit(restore_stdin);
        }
    }
    fflush(stdout);
    p.fd = 0;
    p.events = POLLIN;
    if (poll(&p, 1, 10) <= 0) {
        return PORT_NO_KEY;
    }
    return read(0, &c, 1) == 1 ? c : PORT_EOF;
}
