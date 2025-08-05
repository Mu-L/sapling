/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use std::sync::Arc;
use std::sync::Mutex;

use anyhow::Context;
use anyhow::Error;
use anyhow::Result;
use anyhow::anyhow;
use bytes::Bytes;
use connection_security_checker::ConnectionSecurityChecker;
use context::LoggingContainer;
use context::SessionContainer;
use fbinit::FacebookInit;
use futures::channel::mpsc;
use futures::future::TryFutureExt;
use futures::sink::SinkExt;
use futures::stream::StreamExt;
use futures::stream::TryStreamExt;
use futures_stats::TimedFutureExt;
use hgproto::HgProtoHandler;
use hgproto::sshproto;
use mononoke_api::Mononoke;
use mononoke_api::Repo;
use mononoke_configs::MononokeConfigs;
use qps::Qps;
use rate_limiting::LoadShedResult;
use rate_limiting::Metric;
use rate_limiting::RateLimitEnvironment;
use rate_limiting::Scope;
use repo_client::RepoClient;
use repo_permission_checker::RepoPermissionCheckerRef;
use scribe_ext::Scribe;
use slog::error;
use sshrelay::Stdio;
use stats::prelude::*;
use textwrap::indent;
use time_ext::DurationExt;

use crate::errors::ErrorKind;
use crate::repo_handlers::RepoHandler;
use crate::repo_handlers::repo_handler;

define_stats! {
    prefix = "mononoke.request_handler";
    wireproto_ms:
        histogram(500, 0, 100_000, Average, Sum, Count; P 5; P 25; P 50; P 75; P 95; P 97; P 99),
    request_success: timeseries(Rate, Sum),
    request_failure: timeseries(Rate, Sum),
    request_outcome_permille: timeseries(Average),
}

pub async fn request_handler(
    fb: FacebookInit,
    reponame: String,
    mononoke: Arc<Mononoke<Repo>>,
    configs: Arc<MononokeConfigs>,
    _security_checker: &ConnectionSecurityChecker,
    stdio: Stdio,
    rate_limiter: Option<RateLimitEnvironment>,
    scribe: Scribe,
    qps: Option<Arc<Qps>>,
    readonly: bool,
) -> Result<()> {
    let Stdio {
        stdin,
        stdout,
        stderr,
        metadata,
    } = stdio;

    let handler = repo_handler(mononoke, &reponame).with_context(|| {
        // We don't have a logger yet as the repo hasn't been created yet, so only log to client
        log_error_to_client(
            stderr.clone(),
            "Unknown Repo:",
            &format!("Requested repo \"{reponame}\" does not exist or is disabled"),
        );

        anyhow!("Unknown Repo: {}", &reponame)
    })?;

    let RepoHandler {
        logger,
        mut scuba,
        repo,
        maybe_push_redirector_args,
        repo_client_knobs,
    } = handler;

    // Upgrade log to include server drain
    scuba = scuba.with_seq("seq");
    scuba.add("repo", reponame);
    if let Some(config_info) = configs.config_info().as_ref() {
        scuba.add("config_store_version", config_info.content_hash.clone());
        scuba.add("config_store_last_updated_at", config_info.last_updated_at);
    }
    scuba.add_metadata(&metadata);
    scuba.sample_for_identities(metadata.identities());

    let rate_limiter = rate_limiter.map(|r| r.get_rate_limiter());
    if let Some(ref rate_limiter) = rate_limiter {
        if let LoadShedResult::Fail(err) = {
            let main_client_id = metadata
                .client_info()
                .and_then(|client_info| client_info.request_info.clone())
                .and_then(|request_info| request_info.main_id);
            rate_limiter.check_load_shed(
                metadata.identities(),
                main_client_id.as_deref(),
                &mut scuba,
            )
        } {
            scuba.log_with_msg("Request rejected due to load shedding", format!("{}", err));
            error!(logger, "Request rejected due to load shedding: {}", err);
            log_error_to_client(
                stderr,
                "Request rejected due to load shedding:",
                &format!("{err}"),
            );

            return Err(err.into());
        }
    }

    let is_allowed_to_repo = repo
        .repo_permission_checker()
        .check_if_read_access_allowed(metadata.identities())
        .await;

    if !is_allowed_to_repo {
        let err: Error = ErrorKind::AuthorizationFailed.into();
        scuba.log_with_msg("Authorization failed", format!("{}", err));
        error!(logger, "Authorization failed: {}", err);
        log_error_to_client(stderr, "Authorization failed:", &format!("{err}"));

        return Err(err);
    }

    // Info per wireproto command within this session
    let wireproto_calls = Arc::new(Mutex::new(Vec::new()));

    scuba.log_with_msg("Connection established", None);

    let session_builder = SessionContainer::builder(fb)
        .metadata(metadata.clone())
        .readonly(readonly)
        .rate_limiter(rate_limiter);

    let session = session_builder.build();

    let mut logging = LoggingContainer::new(fb, logger.clone(), scuba.clone());
    logging.with_scribe(scribe);

    let repo_client = RepoClient::new(
        repo,
        session.clone(),
        logging,
        maybe_push_redirector_args,
        repo_client_knobs,
    );
    let request_perf_counters = repo_client.request_perf_counters();

    // Construct a hg protocol handler
    let proto_handler = HgProtoHandler::new(
        logger.clone(),
        stdin,
        repo_client,
        sshproto::HgSshCommandDecode,
        sshproto::HgSshCommandEncode,
        wireproto_calls.clone(),
        qps.clone(),
        metadata.revproxy_region().clone(),
    );

    // send responses back
    let endres = proto_handler
        .into_stream()
        .inspect_ok(move |bytes| {
            session.bump_load(Metric::EgressBytes, Scope::Regional, bytes.len() as f64)
        })
        .map_err(Error::from)
        .map_ok(|b| Bytes::copy_from_slice(b.as_ref()))
        .forward(stdout.sink_map_err(Error::from))
        .map_ok(|_| ());

    // If we got an error at this point, then catch it and print a message
    let (stats, result) = endres.timed().await;

    let wireproto_calls = {
        let mut wireproto_calls = wireproto_calls.lock().expect("lock poisoned");
        std::mem::take(&mut *wireproto_calls)
    };

    STATS::wireproto_ms.add_value(stats.completion_time.as_millis_unchecked() as i64);

    let mut scuba = scuba.clone();

    scuba
        .add_future_stats(&stats)
        .add("wireproto_commands", wireproto_calls);

    // Populate stats no matter what to avoid dead detectors firing.
    STATS::request_success.add_value(0);
    STATS::request_failure.add_value(0);

    // Log request level perf counters
    request_perf_counters.insert_perf_counters(&mut scuba);

    match &result {
        Ok(_) => {
            STATS::request_success.add_value(1);
            STATS::request_outcome_permille.add_value(1000);
            scuba.log_with_msg("Request finished - Success", None)
        }
        Err(err) => {
            if err.is::<mpsc::SendError>() {
                STATS::request_outcome_permille.add_value(0);
                scuba.log_with_msg("Request finished - Client Disconnected", format!("{}", err));
            } else {
                STATS::request_failure.add_value(1);
                STATS::request_outcome_permille.add_value(0);
                scuba.log_with_msg("Request finished - Failure", format!("{:#?}", err));
            }
        }
    }

    if let Err(err) = result {
        error!(logger, "Command failed";
            "error" => ?err,
        );
        // log to client
        log_error_to_client(stderr, "Command failed", &format!("{err:?}"));
    }

    Ok(())
}

pub fn log_error_to_client(
    client: mpsc::UnboundedSender<Bytes>,
    description: &'static str,
    error_msg: &str,
) {
    let msg = indent(error_msg, "    ");
    let error_msg = Bytes::from(format!("{description}\n  Error:\n{msg}"));
    let _ = client.unbounded_send(error_msg);
}
