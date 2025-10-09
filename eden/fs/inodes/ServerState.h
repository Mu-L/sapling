/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

#pragma once

#include <folly/concurrency/memory/ReadMostlySharedPtr.h>
#include <memory>

#include "eden/common/utils/PathFuncs.h"
#include "eden/common/utils/RefPtr.h"
#include "eden/common/utils/UserInfo.h"
#include "eden/fs/config/CachedParsedFileMonitor.h"
#include "eden/fs/model/git/GitIgnoreFileParser.h"

namespace folly {
class EventBase;
class Executor;
} // namespace folly

namespace facebook::eden {

class Clock;
class EdenConfig;
class EdenStats;
class FaultInjector;
class FsEventLogger;
class IScribeLogger;
class InodeAccessLogger;
class NfsServer;
class Notifier;
class PrivHelper;
class ProcessInfoCache;
class ReloadableConfig;
class StructuredLogger;
class TopLevelIgnores;
class UnboundedQueueExecutor;
struct SessionInfo;

using EdenStatsPtr = RefPtr<EdenStats>;

/**
 * ServerState is the testable, dependency injection seam for the inode
 * layer. It includes some platform abstractions like Clock, loggers,
 * and configuration, and state shared across multiple mounts.
 *
 * This is normally owned by the main EdenServer object. However unit
 * tests also create ServerState objects without an
 * EdenServer. ServerState should not contain expensive-to-create
 * objects or they should be abstracted behind an interface so
 * appropriate fakes can be used in tests.
 */
class ServerState {
 public:
  ServerState(
      UserInfo userInfo,
      EdenStatsPtr edenStats,
      SessionInfo sessionInfo,
      std::shared_ptr<PrivHelper> privHelper,
      std::shared_ptr<UnboundedQueueExecutor> threadPool,
      std::shared_ptr<folly::Executor> fsChannelThreadPool,
      std::shared_ptr<Clock> clock,
      std::shared_ptr<ProcessInfoCache> processInfoCache,
      std::shared_ptr<StructuredLogger> structuredLogger,
      std::shared_ptr<StructuredLogger> notificationsStructuredLogger,
      std::shared_ptr<IScribeLogger> scribeLogger,
      std::shared_ptr<ReloadableConfig> reloadableConfig,
      const EdenConfig& initialConfig,
      folly::EventBase* mainEventBase,
      std::shared_ptr<Notifier> notifier,
      bool enableFaultInjection = false,
      std::shared_ptr<InodeAccessLogger> inodeAccessLogger = nullptr);
  ~ServerState();

  /**
   * Set the path to the server's thrift socket.
   *
   * This is called by EdenServer once it has initialized the thrift server.
   */
  void setSocketPath(AbsolutePathPiece path) {
    socketPath_ = path.copy();
  }

  /**
   * Get the path to the server's thrift socket.
   *
   * This is used by the EdenMount to populate the `.eden/socket` special file.
   */
  const AbsolutePath& getSocketPath() const {
    return socketPath_;
  }

  /**
   * Get the EdenStats object that tracks process-wide (rather than per-mount)
   * statistics.
   */
  const EdenStatsPtr& getStats() const {
    return edenStats_;
  }

  const std::shared_ptr<ReloadableConfig>& getReloadableConfig() const {
    return config_;
  }

  /**
   * Get the EdenConfig data.
   */
  folly::ReadMostlySharedPtr<const EdenConfig> getEdenConfig();

  /**
   * Get the TopLevelIgnores. It is based on the system and user git ignore
   * files.
   */
  std::unique_ptr<TopLevelIgnores> getTopLevelIgnores();

  /**
   * Get the UserInfo object describing the user running this edenfs process.
   */
  const UserInfo& getUserInfo() const {
    return userInfo_;
  }

  /**
   * Get the PrivHelper object used to perform operations that require
   * elevated privileges.
   */
  PrivHelper* getPrivHelper() {
    return privHelper_.get();
  }

  /**
   * Get the thread pool.
   *
   * Adding new tasks to this thread pool executor will never block.
   */
  const std::shared_ptr<UnboundedQueueExecutor>& getThreadPool() const {
    return threadPool_;
  }

  /**
   * Get the FS channel thread pool.
   *
   * FS channel requests are intended to run on this thread pool.
   */
  const std::shared_ptr<folly::Executor>& getFsChannelThreadPool() const {
    return fsChannelThreadPool_;
  }

  /**
   * Gets a thread pool for running validation. Validation will read file
   * contents through the filesystem. Reads through the filesystem can call
   * back into EdenFS, so we need to ensure that validation does not block
   * any of the threads that EdenFS uses to serve filesystem operations.
   *
   * It's pretty similar to the invalidation threadpool that the channels use.
   * However, this thread pool also errors when reaches capacity rather than
   * blocking. We want this threadpoool to be bounded because we don't want
   * blocking here to increase memory usage until we OOM. Additionally, we don't
   * want to block because this could block checkout. Validation is an
   * asynchronous action that should not effect EdenFS behavior.
   */
  const std::shared_ptr<folly::Executor>& getValidationThreadPool() const {
    return validationThreadPool_;
  }

  /**
   * Get the Clock.
   */
  const std::shared_ptr<Clock>& getClock() const {
    return clock_;
  }

  const std::shared_ptr<NfsServer>& getNfsServer() const& {
    return nfs_;
  }

  const std::shared_ptr<ProcessInfoCache>& getProcessInfoCache() const {
    return processInfoCache_;
  }

  const std::shared_ptr<StructuredLogger>& getStructuredLogger() const {
    return structuredLogger_;
  }

  const std::shared_ptr<StructuredLogger>& getNotificationsStructuredLogger()
      const {
    return notificationsStructuredLogger_;
  }

  /**
   * Returns a ScribeLogger that can be used to send log events to external
   * long term storage for offline consumption. Prefer this method if the
   * caller needs to own a reference due to lifetime mismatch with the
   * ServerState
   */
  const std::shared_ptr<IScribeLogger>& getScribeLogger() const {
    return scribeLogger_;
  }

  /**
   * Returns a InodeAccessLogger that can be used to send log events to external
   * long term storage for offline consumption. Prefer this method if the
   * caller needs to own a reference due to lifetime mismatch with the
   * ServerState
   */
  const std::shared_ptr<InodeAccessLogger>& getInodeAccessLogger() const {
    return inodeAccessLogger_;
  }

  /**
   * Returns a pointer to the FsEventLogger for logging FS event samples, if the
   * platform supports it. Otherwise, returns nullptr. The caller is responsible
   * for null checking.
   */
  const std::shared_ptr<FsEventLogger>& getFsEventLogger() const {
    return fsEventLogger_;
  }

  FaultInjector& getFaultInjector() {
    return *faultInjector_;
  }

  const std::shared_ptr<Notifier>& getNotifier() {
    return notifier_;
  }

 private:
  AbsolutePath socketPath_;
  UserInfo userInfo_;
  EdenStatsPtr edenStats_;
  std::shared_ptr<PrivHelper> privHelper_;
  std::shared_ptr<UnboundedQueueExecutor> threadPool_;
  std::shared_ptr<folly::Executor> fsChannelThreadPool_;
  std::shared_ptr<folly::Executor> validationThreadPool_;
  std::shared_ptr<Clock> clock_;
  std::shared_ptr<ProcessInfoCache> processInfoCache_;
  std::shared_ptr<StructuredLogger> structuredLogger_;
  std::shared_ptr<StructuredLogger> notificationsStructuredLogger_;
  std::shared_ptr<IScribeLogger> scribeLogger_;
  std::unique_ptr<FaultInjector> const faultInjector_;
  std::shared_ptr<NfsServer> nfs_;

  std::shared_ptr<ReloadableConfig> config_;
  folly::Synchronized<CachedParsedFileMonitor<GitIgnoreFileParser>>
      userIgnoreFileMonitor_;
  folly::Synchronized<CachedParsedFileMonitor<GitIgnoreFileParser>>
      systemIgnoreFileMonitor_;
  std::shared_ptr<Notifier> notifier_;
  std::shared_ptr<InodeAccessLogger> inodeAccessLogger_;
  std::shared_ptr<FsEventLogger> fsEventLogger_;
};
} // namespace facebook::eden
