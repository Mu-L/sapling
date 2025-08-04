/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#pragma once

#include <folly/Range.h>
#include <string>

#include "eden/fs/model/Hash.h"
#include "eden/fs/model/RootId.h"
#include "eden/fs/utils/EdenError.h"

namespace facebook::eden {

/**
 * Convert a Hash to a std::string to be returned via thrift as a thrift
 * BinaryHash data type.
 */
inline std::string thriftHash20(const Hash20& hash) {
  return folly::StringPiece{hash.getBytes()}.str();
}

/**
 * Convert thrift BinaryHash data type into a Hash20 object.
 *
 * This allows the input to be either a 20-byte binary string, or a 40-byte
 * hexadecimal string.
 */
inline Hash20 hash20FromThrift(folly::StringPiece commitID) {
  if (commitID.size() == Hash20::RAW_SIZE) {
    // This looks like 20 bytes of binary data.
    return Hash20(folly::ByteRange(folly::StringPiece(commitID)));
  } else if (commitID.size() == 2 * Hash20::RAW_SIZE) {
    // This looks like 40 bytes of hexadecimal data.
    return Hash20(commitID);
  } else {
    throw newEdenError(
        EINVAL,
        EdenErrorType::ARGUMENT_ERROR,
        "expected argument to be a 20-byte binary hash or "
        "40-byte hexadecimal hash; got \"",
        commitID,
        "\"");
  }
}

inline std::string thriftHash32(const Hash32& hash) {
  return folly::StringPiece{hash.getBytes()}.str();
}

/**
 * Convert thrift BinaryHash data type into a Hash20 object.
 *
 * This allows the input to be either a 32-byte binary string, or a 64-byte
 * hexadecimal string.
 */
inline Hash32 hash32FromThrift(folly::StringPiece commitID) {
  if (commitID.size() == Hash32::RAW_SIZE) {
    // This looks like 32 bytes of binary data.
    return Hash32(folly::ByteRange(folly::StringPiece(commitID)));
  } else if (commitID.size() == 2 * Hash32::RAW_SIZE) {
    // This looks like 64 bytes of hexadecimal data.
    return Hash32(commitID);
  } else {
    throw newEdenError(
        EINVAL,
        EdenErrorType::ARGUMENT_ERROR,
        "expected argument to be a 32-byte binary hash or "
        "64-byte hexadecimal hash; got \"",
        commitID,
        "\"");
  }
}

/**
 * A RootId codec suitable for BackingStores that use 20-byte hashes for
 * RootIds, like Git and Hg.
 */
class HashRootIdCodec : public RootIdCodec {
 public:
  RootId parseRootId(folly::StringPiece piece) override;
  std::string renderRootId(const RootId& rootId) override;
};

inline TimeSpec thriftTimeSpec(const timespec& ts) {
  auto thriftTs = TimeSpec();
  thriftTs.seconds() = ts.tv_sec;
  thriftTs.nanoSeconds() = ts.tv_nsec;
  return thriftTs;
}

} // namespace facebook::eden
