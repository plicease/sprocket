/* SPDX-License-Identifier: MIT */
/*
 * printf and sprintf for the DOS build, which links libc16: only the
 * conversions SPROCKET's messages use, integers and strings.
 *
 *   %d %i %u %x %X %c %s %%, with l for long; the flags - and 0; a width
 *   and a precision, each a number or *.
 *
 * A precision limits %s, and is ignored for the others. There is no
 * floating point, and no +, space or # flag. An unknown conversion is
 * copied as it is.
 *
 * printf writes to stdout in blocks, so a message costs one or two DOS
 * writes even though libc16's streams are unbuffered.
 */
#include <stdarg.h>
#include <stdio.h>
#include <string.h>

#include "dosfmt.h"

struct out {
    char *buf; /* sprintf's buffer, or NULL for stdout */
    int count;
    unsigned used;
    char block[64];
};

static void flush(struct out *o)
{
    if (o->used > 0) {
        fwrite(o->block, 1, o->used, stdout);
        o->used = 0;
    }
}

static void put(struct out *o, char c)
{
    o->count++;
    if (o->buf != NULL) {
        *o->buf++ = c;
        return;
    }
    o->block[o->used++] = c;
    if (o->used == sizeof o->block) {
        flush(o);
    }
}

static void repeat(struct out *o, char c, int n)
{
    while (n-- > 0) {
        put(o, c);
    }
}

/* The number or * at *p, advancing *p past it. */
static int number(const char **p, va_list *ap)
{
    int n = 0;

    if (**p == '*') {
        (*p)++;
        return va_arg(*ap, int);
    }
    while (**p >= '0' && **p <= '9') {
        n = n * 10 + (*(*p)++ - '0');
    }
    return n;
}

static void format(struct out *o, const char *p, va_list *ap)
{
    static const char lower[] = "0123456789abcdef";
    static const char upper[] = "0123456789ABCDEF";
    /* The digits of an unsigned long, last first (10 in decimal on DOS). */
    char digits[sizeof(unsigned long) * 3];
    const char *s;
    const char *set;
    unsigned long v;
    unsigned base;
    int left, zero, width, prec, lng, neg, len;

    for (; *p != '\0'; p++) {
        if (*p != '%') {
            put(o, *p);
            continue;
        }
        p++;
        left = zero = 0;
        for (;; p++) {
            if (*p == '-') {
                left = 1;
            } else if (*p == '0') {
                zero = 1;
            } else {
                break;
            }
        }
        width = number(&p, ap);
        if (width < 0) {
            left = 1;
            width = -width;
        }
        prec = -1;
        if (*p == '.') {
            p++;
            prec = number(&p, ap);
        }
        lng = *p == 'l';
        if (lng) {
            p++;
        }
        switch (*p) {
        case 'c':
            digits[0] = (char)va_arg(*ap, int);
            s = digits;
            len = 1;
            goto text;
        case 's':
            s = va_arg(*ap, const char *);
            if (s == NULL) {
                s = "(null)";
            }
            for (len = 0; (prec < 0 || len < prec) && s[len] != '\0'; len++) {
            }
        text:
            width -= len;
            if (!left) {
                repeat(o, ' ', width);
            }
            while (len-- > 0) {
                put(o, *s++);
            }
            if (left) {
                repeat(o, ' ', width);
            }
            break;
        case 'd':
        case 'i':
        case 'u':
        case 'x':
        case 'X':
            neg = 0;
            if (*p == 'd' || *p == 'i') {
                long n = lng ? va_arg(*ap, long) : va_arg(*ap, int);

                neg = n < 0;
                v = neg ? 0UL - (unsigned long)n : (unsigned long)n;
            } else {
                v = lng ? va_arg(*ap, unsigned long) : va_arg(*ap, unsigned);
            }
            base = *p == 'x' || *p == 'X' ? 16 : 10;
            set = *p == 'X' ? upper : lower;
            len = 0;
            do {
                digits[len++] = set[(unsigned)(v % base)];
                v /= base;
            } while (v != 0);
            width -= len + neg;
            if (!left && !zero) {
                repeat(o, ' ', width);
            }
            if (neg) {
                put(o, '-');
            }
            if (!left && zero) {
                repeat(o, '0', width);
            }
            while (len > 0) {
                put(o, digits[--len]);
            }
            if (left) {
                repeat(o, ' ', width);
            }
            break;
        case '\0':
            return;
        default:
            /* %% and anything unknown: the character itself. */
            put(o, *p);
            break;
        }
    }
}

int printf(const char *fmt, ...)
{
    struct out o;
    va_list ap;

    o.buf = NULL;
    o.count = 0;
    o.used = 0;
    va_start(ap, fmt);
    format(&o, fmt, &ap);
    va_end(ap);
    flush(&o);
    return o.count;
}

int sprintf(char *buf, const char *fmt, ...)
{
    struct out o;
    va_list ap;

    o.buf = buf;
    o.count = 0;
    o.used = 0;
    va_start(ap, fmt);
    format(&o, fmt, &ap);
    va_end(ap);
    *o.buf = '\0';
    return o.count;
}
