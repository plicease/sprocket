/* SPDX-License-Identifier: MIT */
#include <stdlib.h>
#include <string.h>

#include "vectors.h"

int failures = 0;

static void parse_hex(const char *text, struct bytes *b)
{
    char *end;
    unsigned long v;

    b->len = 0;
    for (;;) {
        v = strtoul(text, &end, 16);
        if (end == text) {
            break;
        }
        if (b->len == VEC_MAX_BYTES) {
            printf("fixture value too long\n");
            exit(2);
        }
        b->data[b->len++] = (uint8_t)v;
        text = end;
    }
}

int read_record(FILE *f, struct record *r)
{
    char line[4096];
    char *value;
    int any = 0;

    memset(r, 0, sizeof *r);
    while (fgets(line, sizeof line, f)) {
        line[strcspn(line, "\r\n")] = 0;
        if (line[0] == '#') {
            continue;
        }
        if (line[0] == 0) {
            if (any) {
                return 1;
            }
            continue;
        }
        any = 1;
        value = strchr(line, ' ');
        if (!value) {
            continue;
        }
        *value++ = 0;
        while (*value == ' ') {
            value++;
        }
        if (strcmp(line, "name") == 0) {
            strncpy(r->name, value, sizeof r->name - 1);
        } else if (strcmp(line, "body") == 0) {
            parse_hex(value, &r->body);
        } else if (strcmp(line, "wire") == 0) {
            parse_hex(value, &r->wire);
        } else if (strcmp(line, "crc") == 0) {
            r->crc = (unsigned)strtoul(value, NULL, 16);
            r->has_crc = 1;
        } else if (strcmp(line, "max") == 0) {
            r->max = (unsigned)strtoul(value, NULL, 10);
        } else if (strcmp(line, "expect") == 0) {
            char *rest = strchr(value, ' ');
            int i = r->expect_count++;
            if (i >= VEC_MAX_EXPECT) {
                printf("too many expectations in %s\n", r->name);
                exit(2);
            }
            if (rest) {
                *rest++ = 0;
            }
            strncpy(r->expect[i], value, sizeof r->expect[i] - 1);
            if (strcmp(value, "frame") == 0 && rest) {
                parse_hex(rest, &r->expect_body[i]);
            } else if (rest) {
                strncpy(r->expect[i], rest, sizeof r->expect[i] - 1);
            }
        }
    }
    return any;
}

void find_record(const char *path, const char *name, struct record *r)
{
    FILE *f = fopen(path, "r");

    if (!f) {
        printf("cannot open %s\n", path);
        exit(2);
    }
    while (read_record(f, r)) {
        if (strcmp(r->name, name) == 0) {
            fclose(f);
            return;
        }
    }
    printf("no record %s in %s\n", name, path);
    exit(2);
}
