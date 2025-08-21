#!/usr/bin/env python3
# Copyright (c) Meta Platforms, Inc. and affiliates.
#
# This software may be used and distributed according to the terms of the
# GNU General Public License version 2.

# pyre-unsafe

import subprocess
import sys
import time
from typing import Dict, List, Optional

from eden.fs.service.eden.thrift_types import (
    Added,
    ChangeNotification,
    ChangesSinceV2Params,
    ChangesSinceV2Result,
    Dtype,
    Modified,
    SynchronizeWorkingCopyParams,
)

from facebook.eden.ttypes import (
    ChangeNotification as ChangeNotificationLegacy,
    ChangesSinceV2Params as ChangesSinceV2ParamsLegacy,
    ChangesSinceV2Result as ChangesSinceV2ResultLegacy,
    Dtype as DtypeLegacy,
    SmallChangeNotification as SmallChangeNotificationLegacy,
    SynchronizeWorkingCopyParams as SynchronizeWorkingCopyParamsLegacy,
)

from . import testcase
from .thrift_objects import buildSmallChange
from .thrift_objects_legacy import buildSmallChange as buildSmallChangeLegacy


class JournalTestBaseLegacy(testcase.EdenRepoTest):
    git_test_supported = False

    def edenfs_extra_config(self) -> Optional[Dict[str, List[str]]]:
        configs = super().edenfs_extra_config()
        if configs:
            configs["notify"] = ['max-num-changes = "100"']
        return configs

    def edenfs_logging_settings(self) -> Dict[str, str]:
        return {"eden": "DBG3"}

    def populate_repo(self) -> None:
        # Create the initial repo. It requires at least 1 file and 1 commit
        self.repo.write_file("hello", "bonjour\n")
        self.repo.write_file("the_land/is", "cloaked\n")
        self.commit0 = self.repo.commit("Commit 0.")

    def setUp(self) -> None:
        # needs to be done before set up because these need to be created
        # for populate_repo() and the supers set up will call this.
        self.commit0 = ""
        self.commit1 = ""

        super().setUp()

        self.client = self.get_thrift_client_legacy()
        self.client.open()
        self.addCleanup(self.client.close)

        self.position = self.client.getCurrentJournalPosition(self.mount_path_bytes)

    def check_changes(self, changes, expected_changes) -> bool:
        expected_changes_index = 0
        for change in changes:
            if change == expected_changes[expected_changes_index]:
                expected_changes_index += 1
                if expected_changes_index == len(expected_changes):
                    return True
        print("Expected changes not found:")
        for i in range(expected_changes_index, len(expected_changes)):
            print(expected_changes[i])
        print("in:")
        print(changes)
        return False

    def getChangesSinceV2(
        self,
        position,
        included_roots=None,
        excluded_roots=None,
        included_suffixes=None,
        excluded_suffixes=None,
        root=None,
    ) -> ChangesSinceV2ResultLegacy:
        if sys.platform == "win32":
            # On Windows, we need to wait for the file system to settle before
            # calling getChangesSinceV2. Otherwise, we may get missing results.
            self.client.synchronizeWorkingCopy(
                self.mount_path_bytes, SynchronizeWorkingCopyParamsLegacy()
            )
        return self.client.changesSinceV2(
            ChangesSinceV2ParamsLegacy(
                mountPoint=self.mount_path_bytes,
                fromPosition=position,
                includedRoots=included_roots,
                excludedRoots=excluded_roots,
                includedSuffixes=included_suffixes,
                excludedSuffixes=excluded_suffixes,
                root=root,
            )
        )

    def repo_write_file(self, path, contents, mode=None, add=True) -> None:
        self.eden_repo.write_file(path, contents, mode, add)

    def setup_test_add_file(self) -> ChangesSinceV2ResultLegacy:
        position = self.client.getCurrentJournalPosition(self.mount_path_bytes)
        self.repo_write_file("test_file", "", add=False)
        return self.getChangesSinceV2(position=position)

    def setup_test_add_file_root(self, root) -> ChangesSinceV2ResultLegacy:
        position = self.client.getCurrentJournalPosition(self.mount_path_bytes)
        self.repo_write_file(f"{root}/test_file", "", add=False)
        return self.getChangesSinceV2(position=position, root=root)

    def setup_test_rename_file(self) -> ChangesSinceV2ResultLegacy:
        self.repo_write_file("test_file", "", add=False)
        position = self.client.getCurrentJournalPosition(self.mount_path_bytes)
        self.rename("test_file", "best_file")
        return self.getChangesSinceV2(position=position)

    def repo_rmdir(self, path) -> None:
        self.rmdir(path)

    def add_file_expect(
        self, path, contents, mode=None, add=True
    ) -> List[ChangeNotificationLegacy]:
        self.repo_write_file(path, contents, mode, add)
        return [
            buildSmallChangeLegacy(
                SmallChangeNotificationLegacy.ADDED,
                DtypeLegacy.REGULAR,
                path=path.encode(),
            ),
            buildSmallChangeLegacy(
                SmallChangeNotificationLegacy.MODIFIED,
                DtypeLegacy.REGULAR,
                path=path.encode(),
            ),
        ]

    def add_folder_expect(self, path) -> List[ChangeNotificationLegacy]:
        self.mkdir(path)
        return [
            buildSmallChangeLegacy(
                SmallChangeNotificationLegacy.ADDED, DtypeLegacy.DIR, path=path.encode()
            ),
        ]

    def repo_chmod(self, fd, mode) -> None:
        self.chmod(fd, mode)

    def repo_chown(self, fd) -> None:
        # because chown needs sudo to change to nobody
        fullpath = self.eden_repo.get_path(fd)
        cmd = ["sudo", "chown", "nobody:nobody", fullpath]
        subprocess.call(cmd)


class WindowsJournalTestBaseLegacy(JournalTestBaseLegacy):
    # This class is intended to test the journal system for EdenFS on Windows.
    # This is required because file changes are not immediately reported to Eden,
    # so we need to wait for the journal to update before checking its status
    SYNC_MAX: int = 1  # noqa

    def syncProjFS(self, position) -> None:
        # Wait for eden to get the PrjFS notification
        pollTime = 0.1
        waitTime = 0
        newPosition = self.client.getCurrentJournalPosition(self.mount_path_bytes)
        while position == newPosition and waitTime < self.SYNC_MAX:
            time.sleep(pollTime)
            waitTime += pollTime
            newPosition = self.client.getCurrentJournalPosition(self.mount_path_bytes)

    def repo_write_file(self, path, contents, mode=None, add=True) -> None:
        position = self.client.getCurrentJournalPosition(self.mount_path_bytes)
        super().repo_write_file(path, contents, mode, add)
        self.syncProjFS(position)

    def rm(self, path) -> None:
        position = self.client.getCurrentJournalPosition(self.mount_path_bytes)
        super().rm(path)
        self.syncProjFS(position)

    def rename(self, from_path, to_path) -> None:
        position = self.client.getCurrentJournalPosition(self.mount_path_bytes)
        super().rename(from_path, to_path)
        self.syncProjFS(position)

    def mkdir(self, path) -> None:
        position = self.client.getCurrentJournalPosition(self.mount_path_bytes)
        super().mkdir(path)
        self.syncProjFS(position)

    def repo_rmdir(self, path) -> None:
        position = self.client.getCurrentJournalPosition(self.mount_path_bytes)
        super().rmdir(path)
        self.syncProjFS(position)

    def repo_chmod(self, fd, mode) -> None:
        position = self.client.getCurrentJournalPosition(self.mount_path_bytes)
        self.chmod(fd, mode)
        self.syncProjFS(position)

    def repo_chown(self, fd) -> None:
        position = self.client.getCurrentJournalPosition(self.mount_path_bytes)
        super().repo_chown(fd)
        self.syncProjFS(position)

    def add_file_expect(
        self, path, contents, mode=None, add=True
    ) -> List[ChangeNotificationLegacy]:
        self.repo_write_file(path, contents, mode, add)
        return [
            buildSmallChangeLegacy(
                SmallChangeNotificationLegacy.ADDED,
                DtypeLegacy.REGULAR,
                path=path.encode(),
            ),
        ]


class JournalTestBase(testcase.EdenRepoTest):
    git_test_supported = False

    def edenfs_extra_config(self) -> Optional[Dict[str, List[str]]]:
        configs = super().edenfs_extra_config()
        if configs:
            configs["notify"] = ['max-num-changes = "100"']
        return configs

    def edenfs_logging_settings(self) -> Dict[str, str]:
        return {"eden": "DBG3"}

    def populate_repo(self) -> None:
        # Create the initial repo. It requires at least 1 file and 1 commit
        self.repo.write_file("hello", "bonjour\n")
        self.repo.write_file("the_land/is", "cloaked\n")
        self.commit0 = self.repo.commit("Commit 0.")

    def setUp(self) -> None:
        # needs to be done before set up because these need to be created
        # for populate_repo() and the supers set up will call this.
        self.commit0 = ""
        self.commit1 = ""

        super().setUp()

    def check_changes(
        self,
        actual: ChangeNotification,
        expected: ChangeNotification,
    ) -> bool:
        """Compare two ChangeNotification objects for equality."""
        # Check if both have smallChange
        if hasattr(actual, "smallChange") and actual.smallChange is not None:
            if not (hasattr(expected, "smallChange") and expected.smallChange is None):
                return False
            return self._small_changes_equal(actual.smallChange, expected.smallChange)

        # Check if both have largeChange
        if hasattr(actual, "largeChange") and actual.largeChange is not None:
            if not (
                hasattr(expected, "largeChange") and expected.largeChange is not None
            ):
                return False
            return self._large_changes_equal(actual.largeChange, expected.largeChange)

        # Both should be empty
        return (
            not hasattr(expected, "smallChange") or expected.smallChange is None
        ) and (not hasattr(expected, "largeChange") or expected.largeChange is None)

    def _small_changes_equal(self, actual, expected) -> bool:
        """Compare two SmallChangeNotification objects for equality."""
        # Check added
        if hasattr(actual, "added") and actual.added is not None:
            if not (hasattr(expected, "added") and expected.added is not None):
                return False
            return actual.added == expected.added

        # Check modified
        if hasattr(actual, "modified") and actual.modified is not None:
            if not (hasattr(expected, "modified") and expected.modified is not None):
                return False
            return actual.modified == expected.modified

        # Check renamed
        if hasattr(actual, "renamed") and actual.renamed is not None:
            if not (hasattr(expected, "renamed") and expected.renamed is not None):
                return False
            return actual.renamed == expected.renamed

        # Check replaced
        if hasattr(actual, "replaced") and actual.replaced is not None:
            if not (hasattr(expected, "replaced") and expected.replaced is not None):
                return False
            return actual.replaced == expected.replaced

        # Check removed
        if hasattr(actual, "removed") and actual.removed is not None:
            if not (hasattr(expected, "removed") and expected.removed is not None):
                return False
            return actual.removed == expected.removed

        return False

    def _large_changes_equal(self, actual, expected) -> bool:
        """Compare two LargeChangeNotification objects for equality."""
        # Check directoryRenamed
        if hasattr(actual, "directoryRenamed") and actual.directoryRenamed is not None:
            if not (
                hasattr(expected, "directoryRenamed")
                and expected.directoryRenamed is not None
            ):
                return False
            return actual.directoryRenamed == expected.directoryRenamed

        # Check commitTransition
        if hasattr(actual, "commitTransition") and actual.commitTransition is not None:
            if not (
                hasattr(expected, "commitTransition")
                and expected.commitTransition is not None
            ):
                return False
            return actual.commitTransition == expected.commitTransition

        # Check lostChanges
        if hasattr(actual, "lostChanges") and actual.lostChanges is not None:
            if not (
                hasattr(expected, "lostChanges") and expected.lostChanges is not None
            ):
                return False
            return actual.lostChanges == expected.lostChanges

        return False

    async def getChangesSinceV2(
        self,
        position,
        included_roots=None,
        excluded_roots=None,
        included_suffixes=None,
        excluded_suffixes=None,
        root=None,
    ) -> ChangesSinceV2Result:
        # Convert parameters to correct types for Thrift interface
        # includedRoots and excludedRoots expect bytes (PathString)
        if included_roots is not None:
            included_roots = [
                path.encode() if isinstance(path, str) else path
                for path in included_roots
            ]
        if excluded_roots is not None:
            excluded_roots = [
                path.encode() if isinstance(path, str) else path
                for path in excluded_roots
            ]
        # includedSuffixes and excludedSuffixes expect strings
        if included_suffixes is not None:
            included_suffixes = [
                suffix.decode() if isinstance(suffix, bytes) else suffix
                for suffix in included_suffixes
            ]
        if excluded_suffixes is not None:
            excluded_suffixes = [
                suffix.decode() if isinstance(suffix, bytes) else suffix
                for suffix in excluded_suffixes
            ]
        # root expects bytes (PathString)
        if root is not None and isinstance(root, str):
            root = root.encode()

        async with self.get_thrift_client() as client:
            if sys.platform == "win32":
                # On Windows, we need to wait for the file system to settle before
                # calling getChangesSinceV2. Otherwise, we may get missing results.
                await client.synchronizeWorkingCopy(
                    self.mount_path_bytes, SynchronizeWorkingCopyParams()
                )
            return await client.changesSinceV2(
                ChangesSinceV2Params(
                    mountPoint=self.mount_path_bytes,
                    fromPosition=position,
                    includedRoots=included_roots,
                    excludedRoots=excluded_roots,
                    includedSuffixes=included_suffixes,
                    excludedSuffixes=excluded_suffixes,
                    root=root,
                )
            )

    async def repo_write_file(self, path, contents, mode=None, add=True) -> None:
        self.eden_repo.write_file(path, contents, mode, add)

    async def setup_test_add_file(self) -> ChangesSinceV2Result:
        async with self.get_thrift_client() as client:
            position = await client.getCurrentJournalPosition(self.mount_path_bytes)
            await self.repo_write_file("test_file", "", add=False)
            return await self.getChangesSinceV2(position=position)

    async def setup_test_add_file_root(self, root) -> ChangesSinceV2Result:
        async with self.get_thrift_client() as client:
            position = await client.getCurrentJournalPosition(self.mount_path_bytes)
            # Handle both string and bytes root parameter
            root_str = root.decode() if isinstance(root, bytes) else root
            await self.repo_write_file(f"{root_str}/test_file", "", add=False)
            return await self.getChangesSinceV2(position=position, root=root)

    async def setup_test_rename_file(self) -> ChangesSinceV2Result:
        await self.repo_write_file("test_file", "", add=False)
        async with self.get_thrift_client() as client:
            position = await client.getCurrentJournalPosition(self.mount_path_bytes)
            await self.rename_async("test_file", "best_file")
            return await self.getChangesSinceV2(position=position)

    async def repo_rmdir(self, path) -> None:
        self.rmdir(path)

    async def add_file_expect(
        self, path, contents, mode=None, add=True
    ) -> List[ChangeNotification]:
        await self.repo_write_file(path, contents, mode, add)
        return [
            buildSmallChange(Added, Dtype.REGULAR, path=path.encode()),
            buildSmallChange(Modified, Dtype.REGULAR, path=path.encode()),
        ]

    async def add_folder_expect(self, path) -> List[ChangeNotification]:
        self.mkdir(path)
        return [
            buildSmallChange(Added, Dtype.DIR, path=path.encode()),
        ]

    async def repo_chmod(self, fd, mode) -> None:
        self.chmod(fd, mode)

    async def repo_chown(self, fd) -> None:
        # because chown needs sudo to change to nobody
        fullpath = self.eden_repo.get_path(fd)
        cmd = ["sudo", "chown", "nobody:nobody", fullpath]
        subprocess.call(cmd)

    async def rm_async(self, path) -> None:
        # Async wrapper, supporting WindowsJournalTestBase
        super().rm(path)

    async def rename_async(self, from_path, to_path) -> None:
        # Async wrapper, supporting WindowsJournalTestBase
        super().rename(from_path, to_path)

    async def mkdir_async(self, path) -> None:
        # Async wrapper, supporting WindowsJournalTestBase
        super().mkdir(path)


class WindowsJournalTestBase(JournalTestBase):
    # This class is intended to test the journal system for EdenFS on Windows.
    # This is required because file changes are not immediately reported to Eden,
    # so we need to wait for the journal to update before checking its status
    SYNC_MAX: int = 1  # noqa

    async def syncProjFS(self, position) -> None:
        # Wait for eden to get the PrjFS notification
        pollTime = 0.1
        waitTime = 0
        async with self.get_thrift_client() as client:
            newPosition = await client.getCurrentJournalPosition(self.mount_path_bytes)
            while position == newPosition and waitTime < self.SYNC_MAX:
                time.sleep(pollTime)
                waitTime += pollTime
                newPosition = await client.getCurrentJournalPosition(
                    self.mount_path_bytes
                )

    async def repo_write_file(self, path, contents, mode=None, add=True) -> None:
        async with self.get_thrift_client() as client:
            position = await client.getCurrentJournalPosition(self.mount_path_bytes)
            self.eden_repo.write_file(path, contents, mode, add)
            await self.syncProjFS(position)

    async def rm_async(self, path) -> None:
        async with self.get_thrift_client() as client:
            position = await client.getCurrentJournalPosition(self.mount_path_bytes)
            testcase.EdenTestCase.rm(self, path)
            await self.syncProjFS(position)

    async def rename_async(self, from_path, to_path) -> None:
        async with self.get_thrift_client() as client:
            position = await client.getCurrentJournalPosition(self.mount_path_bytes)
            testcase.EdenTestCase.rename(self, from_path, to_path)
            await self.syncProjFS(position)

    async def mkdir_async(self, path) -> None:
        async with self.get_thrift_client() as client:
            position = await client.getCurrentJournalPosition(self.mount_path_bytes)
            testcase.EdenTestCase.mkdir(self, path)
            await self.syncProjFS(position)

    async def repo_rmdir(self, path) -> None:
        async with self.get_thrift_client() as client:
            position = await client.getCurrentJournalPosition(self.mount_path_bytes)
            self.rmdir(path)
            await self.syncProjFS(position)

    async def repo_chmod(self, fd, mode) -> None:
        async with self.get_thrift_client() as client:
            position = await client.getCurrentJournalPosition(self.mount_path_bytes)
            self.chmod(fd, mode)
            await self.syncProjFS(position)

    async def repo_chown(self, fd) -> None:
        async with self.get_thrift_client() as client:
            position = await client.getCurrentJournalPosition(self.mount_path_bytes)
            fullpath = self.eden_repo.get_path(fd)
            cmd = ["sudo", "chown", "nobody:nobody", fullpath]
            subprocess.call(cmd)
            await self.syncProjFS(position)

    async def add_file_expect(
        self, path, contents, mode=None, add=True
    ) -> List[ChangeNotification]:
        await self.repo_write_file(path, contents, mode, add)
        return [
            buildSmallChange(Added, Dtype.REGULAR, path=path.encode()),
        ]

    async def add_folder_expect(self, path) -> List[ChangeNotification]:
        self.mkdir(path)
        return [
            buildSmallChange(Added, Dtype.DIR, path=path.encode()),
        ]
