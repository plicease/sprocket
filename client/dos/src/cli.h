/* SPDX-License-Identifier: MIT */
/*
 * SPROCKET CLIENT: the interactive native shell, the DOS counterpart of
 * `sprocket client` on Linux (docs/sprocket-file-transfer-ui.md).
 */
#ifndef SPROCKET_CLI_H
#define SPROCKET_CLI_H

#include <stdint.h>

/*
 * Call the server on the open port, run the shell until the user quits,
 * and close the session. tx and rx are buffers for req_init. Returns the
 * program's exit status.
 */
int cli_run(const char *port, long baud, uint8_t *tx, uint8_t *rx, uint16_t max_payload,
            uint16_t wait);

#endif
