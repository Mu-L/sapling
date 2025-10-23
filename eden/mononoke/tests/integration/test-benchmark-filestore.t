# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License found in the LICENSE file in the root
# directory of this source tree.

  $ . "${TEST_FIXTURES}/library.sh"

# Basic smoke test

  $ setup_common_config
  $ echo "foobar" > "${TESTTMP}/foo"

  $ GLOG_minloglevel=5 "$MONONOKE_BENCHMARK_FILESTORE" "${CACHE_ARGS[@]}" "${COMMON_ARGS[@]}" --mononoke-config-path "$TESTTMP"/mononoke-config "${TESTTMP}/foo" memory
  Test with FilestoreConfig { * }, writing into NoopKeyedBlobstore { * } (glob)
  Write start: 7 B
  Success: * (glob)
  Write committed: "content.blake2.e8ab2cbe03f03318289331d6e7c3173dbb530cce996f94208d86e7421e5c3f28"
  Fetch start: Canonical(ContentId(Blake2(e8ab2cbe03f03318289331d6e7c3173dbb530cce996f94208d86e7421e5c3f28))) (7 B)
  Success: * (glob)
  Fetch start: Canonical(ContentId(Blake2(e8ab2cbe03f03318289331d6e7c3173dbb530cce996f94208d86e7421e5c3f28))) (7 B)
  Success: * (glob)
