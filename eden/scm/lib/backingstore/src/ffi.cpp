/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#include <folly/Try.h>
#include <folly/io/IOBuf.h>
#include <memory>

#include "eden/fs/model/Tree.h"
#include "eden/fs/model/TreeFwd.h"
#include "eden/scm/lib/backingstore/include/ffi.h"
#include "eden/scm/lib/backingstore/src/ffi.rs.h" // @manual

namespace sapling {

void sapling_backingstore_get_tree_batch_handler(
    std::shared_ptr<GetTreeBatchResolver> resolver,
    size_t index,
    rust::String error,
    std::unique_ptr<TreeBuilder> builder) {
  using ResolveResult = folly::Try<facebook::eden::TreePtr>;

  resolver->resolve(
      index, folly::makeTryWith([&] {
        if (error.empty()) {
          facebook::eden::TreePtr tree = builder->build();
          if (tree) {
            return ResolveResult{tree};
          } else {
            return ResolveResult{SaplingFetchError{"no tree found"}};
          }
        } else {
          return ResolveResult{SaplingFetchError{std::string(error)}};
        }
      }));
}

void sapling_backingstore_get_tree_aux_batch_handler(
    std::shared_ptr<GetTreeAuxBatchResolver> resolver,
    size_t index,
    rust::String error,
    std::shared_ptr<TreeAuxData> aux) {
  using ResolveResult = folly::Try<std::shared_ptr<TreeAuxData>>;

  resolver->resolve(
      index, folly::makeTryWith([&] {
        if (error.empty()) {
          return ResolveResult{aux};
        } else {
          return ResolveResult{SaplingFetchError{std::string(error)}};
        }
      }));
}

void sapling_backingstore_get_blob_batch_handler(
    std::shared_ptr<GetBlobBatchResolver> resolver,
    size_t index,
    rust::String error,
    std::unique_ptr<folly::IOBuf> blob) {
  using ResolveResult = folly::Try<std::unique_ptr<folly::IOBuf>>;

  resolver->resolve(
      index,
      folly::makeTryWith(
          [blob = std::move(blob), error = std::move(error)]() mutable {
            if (error.empty()) {
              return ResolveResult{std::move(blob)};
            } else {
              return ResolveResult{
                  SaplingFetchError{std::string(std::move(error))}};
            }
          }));
}

void sapling_backingstore_get_file_aux_batch_handler(
    std::shared_ptr<GetFileAuxBatchResolver> resolver,
    size_t index,
    rust::String error,
    std::shared_ptr<FileAuxData> aux) {
  using ResolveResult = folly::Try<std::shared_ptr<FileAuxData>>;

  resolver->resolve(
      index, folly::makeTryWith([&] {
        if (error.empty()) {
          return ResolveResult{aux};
        } else {
          return ResolveResult{SaplingFetchError{std::string(error)}};
        }
      }));
}

void TreeBuilder::add_entry(
    rust::Str name,
    const std::array<uint8_t, 20>& hg_node,
    facebook::eden::TreeEntryType ttype) {
  emplace_entry(
      facebook::eden::PathComponent{
          std::string_view{name.data(), name.length()}},
      facebook::eden::TreeEntry{
          make_entry_oid(hg_node, name),
          ttype,
          std::nullopt,
          std::nullopt,
          std::nullopt,
      });
}

void TreeBuilder::add_entry_with_aux_data(
    rust::Str name,
    const std::array<uint8_t, 20>& hg_node,
    facebook::eden::TreeEntryType ttype,
    const uint64_t size,
    const std::array<uint8_t, 20>& sha1,
    const std::array<uint8_t, 32>& blake3) {
  emplace_entry(
      facebook::eden::PathComponent{
          std::string_view{name.data(), name.length()}},
      facebook::eden::TreeEntry{
          make_entry_oid(hg_node, name),
          ttype,
          size,
          std::optional<facebook::eden::Hash20>(sha1),
          std::optional<facebook::eden::Hash32>(blake3),
      });
}

void TreeBuilder::emplace_entry(
    facebook::eden::PathComponent&& name,
    facebook::eden::TreeEntry&& entry) {
  if (entry.isTree()) {
    numDirs_++;
  } else {
    numFiles_++;
  }

  entries_.emplace_back(std::move(name), std::move(entry));
}

facebook::eden::ObjectId TreeBuilder::make_entry_oid(
    const std::array<uint8_t, 20>& hg_node,
    rust::Str name) {
  // Construct the entry's oid.
  auto piece = facebook::eden::PathComponentPiece{
      std::string_view{name.data(), name.length()}};
  facebook::eden::RelativePath fullPath =
      facebook::eden::operator+(path_, piece);
  return facebook::eden::HgProxyHash::store(
      fullPath, facebook::eden::Hash20{hg_node}, objectIdFormat_);
}

void TreeBuilder::set_aux_data(
    const std::array<uint8_t, 32>& digest,
    uint64_t size) {
  auxData_ = std::make_shared<facebook::eden::TreeAuxDataPtr::element_type>(
      facebook::eden::Hash32{digest}, size);
}

facebook::eden::TreePtr TreeBuilder::build() {
  if (missing_) {
    return nullptr;
  }
  return std::make_shared<facebook::eden::TreePtr::element_type>(
      std::move(oid_),
      facebook::eden::Tree::container{std::move(entries_), caseSensitive_},
      std::move(auxData_));
}

std::unique_ptr<TreeBuilder> new_builder(
    bool caseSensitive,
    facebook::eden::HgObjectIdFormat oidFormat,
    const rust::Slice<const uint8_t> oid,
    const rust::Slice<const uint8_t> path) {
  return std::make_unique<TreeBuilder>(TreeBuilder{
      facebook::eden::ObjectId{folly::ByteRange{oid.data(), oid.size()}},
      facebook::eden::RelativePathPiece{std::string_view{
          reinterpret_cast<const char*>(path.data()), path.size()}},
      caseSensitive ? facebook::eden::CaseSensitivity::Sensitive
                    : facebook::eden::CaseSensitivity::Insensitive,
      oidFormat,
  });
}

} // namespace sapling
