/* SPDX-License-Identifier: MIT */
/*
 * Host platform layer for testing the client on Linux: a tty or PTY in raw
 * mode. Esc is not read; stop it with Ctrl-C.
 */
#define _DEFAULT_SOURCE
#include <errno.h>
#include <fcntl.h>
#include <poll.h>
#include <stdio.h>
#include <string.h>
#include <termios.h>
#include <time.h>
#include <unistd.h>

#include "port.h"

static int fd = -1;

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
