/**
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is licensed under the MIT license found in the
 * LICENSE file in the root directory of this source tree.
 */

import * as stylex from '@stylexjs/stylex';
import {Button, buttonStyles} from 'isl-components/Button';
import {ButtonDropdown, styles} from 'isl-components/ButtonDropdown';
import {Row} from 'isl-components/Flex';
import {Icon} from 'isl-components/Icon';
import {Tooltip} from 'isl-components/Tooltip';
import {getZoomLevel} from 'isl-components/zoom';
import {atom, useAtomValue} from 'jotai';
import {loadable} from 'jotai/utils';
import {useEffect, useMemo, useRef, useState} from 'react';
import {useContextMenu} from 'shared/ContextMenu';
import {tracker} from '../analytics';
import serverAPI from '../ClientToServerAPI';
import {bulkFetchFeatureFlags, useFeatureFlagSync} from '../featureFlags';
import {t} from '../i18n';
import {Internal} from '../Internal';
import platform from '../platform';
import {optimisticMergeConflicts} from '../previews';
import {repositoryInfo} from '../serverAPIState';
import type {CommitInfo, PlatformSpecificClientToServerMessages} from '../types';
import {smartActionsConfig} from './actionConfigs';
import {bumpSmartAction, useSortedActions} from './smartActionsOrdering';
import type {ActionContext, ActionMenuItem, SmartActionConfig} from './types';

const smartActionFeatureFlagsAtom = atom<Promise<Record<string, boolean>>>(async () => {
  const flags: Record<string, boolean> = {};

  const flagNames: string[] = [];
  for (const config of smartActionsConfig) {
    if (config.featureFlag && Internal.featureFlags?.[config.featureFlag]) {
      flagNames.push(Internal.featureFlags[config.featureFlag]);
    }
  }

  if (flagNames.length === 0) {
    return flags;
  }

  const results = await bulkFetchFeatureFlags(flagNames);

  // Map back from flag names to flag keys
  for (const config of smartActionsConfig) {
    if (config.featureFlag && Internal.featureFlags?.[config.featureFlag]) {
      const flagName = Internal.featureFlags[config.featureFlag];
      flags[config.featureFlag as string] = results[flagName] ?? false;
    }
  }

  return flags;
});

const loadableFeatureFlagsAtom = loadable(smartActionFeatureFlagsAtom);

export function SmartActionsDropdown({commit}: {commit?: CommitInfo}) {
  const smartActionsMenuEnabled = useFeatureFlagSync(Internal.featureFlags?.SmartActionsMenu);
  const repo = useAtomValue(repositoryInfo);
  const conflicts = useAtomValue(optimisticMergeConflicts);
  const featureFlagsLoadable = useAtomValue(loadableFeatureFlagsAtom);
  const dropdownButtonRef = useRef<HTMLButtonElement>(null);

  const context: ActionContext = useMemo(
    () => ({
      commit,
      repoPath: repo?.repoRoot,
      conflicts,
    }),
    [commit, repo?.repoRoot, conflicts],
  );

  const availableActionItems = useMemo(() => {
    const featureFlagResults =
      featureFlagsLoadable.state === 'hasData' ? featureFlagsLoadable.data : {};
    const items: ActionMenuItem[] = [];

    if (featureFlagsLoadable.state === 'hasData') {
      for (const config of smartActionsConfig) {
        if (
          shouldShowSmartAction(
            config,
            context,
            config.featureFlag ? featureFlagResults[config.featureFlag as string] : true,
          )
        ) {
          items.push({
            id: config.id,
            label: config.label,
            config,
          });
        }
      }
    }

    return items;
  }, [featureFlagsLoadable, context]);

  const sortedActionItems = useSortedActions(availableActionItems);

  const [selectedAction, setSelectedAction] = useState<ActionMenuItem | undefined>(undefined);

  // Update selected action when available actions change
  useEffect(() => {
    setSelectedAction(sortedActionItems[0]);
  }, [sortedActionItems]);

  const contextMenu = useContextMenu(() =>
    sortedActionItems.map(actionItem => ({
      label: (
        // Mark the current action as selected
        <Row>
          <Icon icon={actionItem.id === selectedAction?.id ? 'check' : 'blank'} />
          {actionItem.label}
        </Row>
      ),
      onClick: () => {
        setSelectedAction(actionItem);
      },
      tooltip: actionItem.config.description ? t(actionItem.config.description) : undefined,
    })),
  );

  if (featureFlagsLoadable.state !== 'hasData') {
    return null;
  }

  if (
    !smartActionsMenuEnabled ||
    !Internal.showSmartActions ||
    sortedActionItems.length === 0 ||
    !selectedAction
  ) {
    return null;
  }

  let buttonComponent;

  if (sortedActionItems.length === 1) {
    const singleButton = (
      <Button kind="icon" onClick={() => runSmartAction(sortedActionItems[0].config, context)}>
        <Icon icon="lightbulb-sparkle" />
        {sortedActionItems[0].label}
      </Button>
    );
    buttonComponent = selectedAction.config.description ? (
      <Tooltip title={t(selectedAction.config.description)}>{singleButton}</Tooltip>
    ) : (
      singleButton
    );
  } else {
    buttonComponent = (
      <ButtonDropdown
        kind="icon"
        options={[]}
        selected={selectedAction}
        icon={<Icon icon="lightbulb-sparkle" />}
        onClick={action => {
          runSmartAction(action.config, context);
          // Update the cache with the most recent action
          bumpSmartAction(action.id);
        }}
        onChangeSelected={() => {}}
        customSelectComponent={
          <Button
            {...stylex.props(styles.select, buttonStyles.icon, styles.iconSelect)}
            onClick={e => {
              if (dropdownButtonRef.current) {
                const rect = dropdownButtonRef.current.getBoundingClientRect();
                const zoom = getZoomLevel();
                const xOffset = 4 * zoom;
                const centerX = rect.left + rect.width / 2 - xOffset;
                // Position arrow at the top or bottom edge of button depending on which half of screen we're in
                const isTopHalf =
                  (rect.top + rect.height / 2) / zoom <= window.innerHeight / zoom / 2;
                const yOffset = 5 * zoom;
                const edgeY = isTopHalf ? rect.bottom - yOffset : rect.top + yOffset;
                Object.defineProperty(e, 'clientX', {value: centerX, configurable: true});
                Object.defineProperty(e, 'clientY', {value: edgeY, configurable: true});
              }
              contextMenu(e);
              e.stopPropagation();
            }}
            ref={dropdownButtonRef}
          />
        }
        primaryTooltip={
          selectedAction.config.description
            ? {title: t(selectedAction.config.description)}
            : undefined
        }
      />
    );
  }

  return buttonComponent;
}

function shouldShowSmartAction(
  config: SmartActionConfig,
  context: ActionContext,
  passesFeatureFlag: boolean,
): boolean {
  if (!passesFeatureFlag) {
    return false;
  }

  if (config.platformRestriction && !config.platformRestriction?.includes(platform.platformName)) {
    return false;
  }

  return config.shouldShow?.(context) ?? true;
}

function runSmartAction(config: SmartActionConfig, context: ActionContext): void {
  tracker.track('SmartActionClicked', {extras: {action: config.trackEventName}});
  if (config.getMessagePayload) {
    const payload = config.getMessagePayload(context);
    serverAPI.postMessage({
      ...payload,
    } as PlatformSpecificClientToServerMessages);
  }
}
