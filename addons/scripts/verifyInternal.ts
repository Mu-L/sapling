/**
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

import {Internal} from './Internal';

/* eslint-disable no-console */

/**
 * This script is run during verify-addons-folder.py to validate any internal files.
 * This is a noop in OSS.
 */
async function main() {
  await Internal.validateApiTypeFile?.();
}

main().catch(error => {
  console.error(error);
  process.exit(1);
});
