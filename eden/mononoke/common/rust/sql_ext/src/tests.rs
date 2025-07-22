/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This software may be used and distributed according to the terms of the
 * GNU General Public License version 2.
 */

use anyhow::Result;
use clientinfo::ClientEntryPoint;
use clientinfo::ClientInfo;
use clientinfo::ClientRequestInfo;
use fbinit::FacebookInit;
use metadata::Metadata;
use mononoke_macros::mononoke;
use mononoke_types::RepositoryId;
use sql_query_config::SqlQueryConfig;
use sql_query_telemetry::SqlQueryTelemetry;

use crate::Transaction;
use crate::mononoke_queries;

mononoke_queries! {
    read TestQuery(param_str: String, param_uint: u64) -> (u64, Option<i32>, String, i64) {
        "SELECT 44, NULL, {param_str}, {param_uint}"
    }
    pub(crate) cacheable read TestQuery2() -> (u64, Option<String>) {
        "SELECT 44, NULL"
    }
    pub(super) write TestQuery3(values: (
        val1: i32,
    )) {
        none,
        "INSERT INTO my_table (num, str) VALUES {values}"
    }
    write TestQuery4(id: &str) {
        none,
        mysql("DELETE FROM my_table where id = {id}")
        sqlite("DELETE FROM mytable2 where id = {id}")
    }

    // Test to cover fetching the id from the type, not arg name
    read ReadQuery1(id: RepositoryId) -> (i64) {
        "SELECT x FROM mononoke_queries_test WHERE ID > {id} LIMIT 10"
    }

    write WriteQuery1(values: (x: i64)) {
        none,
        "INSERT INTO mononoke_queries_test (x) VALUES {values}"
    }

    // Test to cover fetching two repo ids
    read ReadQuery2(small_repo_id: RepositoryId, large_repo_id: RepositoryId) -> (i64) {
        "SELECT x FROM mononoke_queries_test WHERE ID > {small_repo_id} AND ID > {large_repo_id} LIMIT 10"
    }
}

#[cfg(fbcode_build)]
#[cfg(test)]
mod facebook {
    use std::collections::HashMap;
    use std::collections::HashSet;

    use itertools::Itertools;
    use maplit::hashmap;
    use maplit::hashset;
    use sql::mysql::MysqlQueryTelemetry;
    use sql_tests_lib::mysql_test_lib::setup_mysql_test_connection;

    use super::*;
    use crate::telemetry::TelemetryGranularity;

    struct TelemetryTestData {
        connection: sql::Connection,
        sql_query_tel: SqlQueryTelemetry,
        cri: ClientRequestInfo,
        temp_path: String,
    }

    #[derive(Debug, Clone, serde::Deserialize, PartialEq)]
    struct ScubaTelemetryLogSample {
        mysql_telemetry: MysqlQueryTelemetry,
        success: bool,
        repo_ids: Vec<RepositoryId>,
        granularity: TelemetryGranularity,
        query_name: Option<String>,
        transaction_query_names: Vec<String>,
    }

    #[mononoke::fbinit_test]
    async fn test_basic_scuba_logging(fb: FacebookInit) -> Result<()> {
        let TelemetryTestData {
            connection,
            sql_query_tel,
            cri,
            temp_path,
        } = setup_scuba_logging_test(fb).await?;

        let _res =
            WriteQuery1::query(&connection, sql_query_tel.clone(), &[(&1i64,), (&2i64,)]).await?;

        let expected_repo_id = 1;
        let _res = ReadQuery1::query(
            &connection,
            sql_query_tel,
            &RepositoryId::new(expected_repo_id),
        )
        .await?;

        // Values that we expect to always be the same.
        let expected_values: HashMap<String, serde_json::Value> = hashmap! {
              "client_correlator" => serde_json::Value::String(cri.correlator.to_string()),
              "client_entry_point" => serde_json::Value::String(ClientEntryPoint::Tests.to_string()),
              "repo_id" => serde_json::Value::Number(expected_repo_id.into()),
        }
        .into_iter()
        .map(|(k, v)| (String::from(k), v))
        .collect();

        // Columns expected to be logged in all samples.
        let expected_in_all: HashSet<String> = hashset! {
          "avg_rru",
          "build_revision",
          "build_rule",
          "client_correlator",
          "client_entry_point",
          "client_identities",
          "client_main_id",
          "cpu_rru",
          "datacenter",
          "delay_rru",
          "delay_rru",
          "full_delay_rru",
          "granularity",
          "instance_type",
          "max_rru",
          "min_rru",
          "overlimit_delay_rru",
          "region_datacenter_prefix",
          "region",
          "server_hostname",
          "session_uuid",
          "some_delay_rru",
          "success",
          "task_full_delay_rru",
          "task_some_delay_rru",
          "time",
        }
        .into_iter()
        .map(String::from)
        .collect();

        // Columns expected in some samples, but not necessarily all.
        let expected_in_some: HashSet<String> = hashset! {
            "query_name",
            "read_tables",
            "signal_time_ENQUEUE",
            "wait_count_ENQUEUE",
            "wait_time_ENQUEUE",
            "write_tables",
            "repo_ids",
        }
        .into_iter()
        .map(String::from)
        .collect();

        // Read the temp file and print its content
        let content = std::fs::read_to_string(&temp_path)?;

        // Extract and print all columns from the scuba logs
        let columns = extract_all_scuba_columns(&content, expected_in_all, expected_values);

        // For debugging purposes. By default will only print if test fails.
        println!("All columns logged in scuba samples: {:#?}", columns);

        assert!(
            expected_in_some.is_subset(&columns),
            "Expected columns that should be in at least one sample are missing"
        );

        Ok(())
    }

    #[mononoke::fbinit_test]
    async fn test_transaction_scuba_logging(fb: FacebookInit) -> Result<()> {
        let TelemetryTestData {
            connection,
            sql_query_tel,
            temp_path,
            ..
        } = setup_scuba_logging_test(fb).await?;

        let _res =
            WriteQuery1::query(&connection, sql_query_tel.clone(), &[(&1i64,), (&2i64,)]).await?;

        let txn = Transaction::new(
            connection.start_transaction().await?,
            Default::default(),
            sql_query_tel.clone(),
        );
        let txn = txn.add_sql_query_tel(sql_query_tel.clone());

        // Query with Repo ID 1
        let (txn, _res) = ReadQuery1::query_with_transaction(txn, &RepositoryId::new(1)).await?;
        // Query with Repo ID 2
        let (txn, _res) =
            ReadQuery2::query_with_transaction(txn, &RepositoryId::new(2), &RepositoryId::new(3))
                .await?;

        txn.commit().await?;

        let scuba_logs = deserialize_scuba_log_file(&temp_path)?;

        println!("scuba_logs: {:#?}", scuba_logs);

        // In the test function:
        let expected_logs = vec![
            ScubaTelemetryLogSample {
                success: true,
                repo_ids: vec![],
                granularity: TelemetryGranularity::Query,
                query_name: Some("WriteQuery1".to_string()),
                mysql_telemetry: MysqlQueryTelemetry {
                    read_tables: hashset! {},
                    write_tables: hashset! {"mononoke_queries_test".to_string()},
                    ..Default::default()
                },
                transaction_query_names: vec![],
            },
            ScubaTelemetryLogSample {
                success: true,
                repo_ids: vec![1.into()],
                granularity: TelemetryGranularity::TransactionQuery,
                query_name: Some("ReadQuery1".to_string()),
                mysql_telemetry: MysqlQueryTelemetry {
                    read_tables: hashset! {"mononoke_queries_test".to_string()},
                    write_tables: hashset! {},
                    ..Default::default()
                },
                transaction_query_names: vec![],
            },
            ScubaTelemetryLogSample {
                success: true,
                repo_ids: vec![2.into(), 3.into()],
                granularity: TelemetryGranularity::TransactionQuery,
                query_name: Some("ReadQuery2".to_string()),
                mysql_telemetry: MysqlQueryTelemetry {
                    read_tables: hashset! {"mononoke_queries_test".to_string()},
                    write_tables: hashset! {},
                    ..Default::default()
                },
                transaction_query_names: vec![],
            },
            // TODO(T223577767): test transaction-level metadata, e.g. run multiple queries
            // for different repos and ensure they are all logged together.
            // Expect a new sample for transaction level
            ScubaTelemetryLogSample {
                success: true,
                repo_ids: vec![1.into(), 2.into(), 3.into()],
                granularity: TelemetryGranularity::Transaction,
                query_name: None,
                mysql_telemetry: MysqlQueryTelemetry {
                    read_tables: hashset! {"mononoke_queries_test".to_string()},
                    write_tables: hashset! {},
                    ..Default::default()
                },
                transaction_query_names: vec!["ReadQuery1", "ReadQuery2"]
                    .into_iter()
                    .map(String::from)
                    .sorted()
                    .collect::<Vec<String>>(),
            },
        ];

        pretty_assertions::assert_eq!(scuba_logs, expected_logs);

        Ok(())
    }

    async fn setup_scuba_logging_test(fb: FacebookInit) -> Result<TelemetryTestData> {
        // Set log file in SQL_TELEMETRY_SCUBA_FILE_PATH environment variable
        let temp_file = tempfile::NamedTempFile::new()?;
        let temp_path = temp_file.path().to_str().unwrap().to_string();
        unsafe {
            std::env::set_var("SQL_TELEMETRY_SCUBA_FILE_PATH", &temp_path);
        }

        let connection: sql::Connection = setup_mysql_test_connection(
            fb,
            "CREATE TABLE IF NOT EXISTS mononoke_queries_test(
                     x INT,
                     y DATETIME NOT NULL DEFAULT CURRENT_TIMESTAMP,
                     test CHAR(64),
                     id INT AUTO_INCREMENT,
                     PRIMARY KEY(id)
                 )",
        )
        .await?;
        let client_info = ClientInfo::new_with_entry_point(ClientEntryPoint::Tests)?;
        let cri = client_info
            .request_info
            .clone()
            .expect("client request info missing");

        println!("cri: {:#?}", cri);

        let mut metadata = Metadata::default();
        metadata.add_client_info(client_info);

        let sql_query_tel = SqlQueryTelemetry::new(fb, metadata);

        Ok(TelemetryTestData {
            connection,
            sql_query_tel,
            cri,
            temp_path,
        })
    }

    /// Extracts all column names from scuba samples in the log content
    fn extract_all_scuba_columns(
        log_content: &str,
        expected_in_all: HashSet<String>,
        expected_values: HashMap<String, serde_json::Value>,
    ) -> HashSet<String> {
        log_content
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .fold(HashSet::new(), |mut all_columns, json| {
            let sample_columns = extract_columns_from_sample(&json, &expected_values);

            assert!(
                expected_in_all.is_subset(&sample_columns),
                "Expected columns that should be in all samples are missing: {0:#?}. Sample: {1:#?}",
                expected_in_all
                    .difference(&sample_columns)
                    .collect::<Vec<_>>(),
                    log_content
            );

            all_columns.extend(sample_columns);
            all_columns
        })
    }

    /// Extracts column names from a single scuba sample
    fn extract_columns_from_sample(
        sample: &serde_json::Value,
        expected_values: &HashMap<String, serde_json::Value>,
    ) -> HashSet<String> {
        // Check each category (normal, int, double, normvector)
        if let Some(obj) = sample.as_object() {
            return obj
                .iter()
                .fold(HashSet::new(), |mut acc, (_category, value)| {
                    if let Some(category_obj) = value.as_object() {
                        expected_values.iter().for_each(|(exp_key, exp_v)| {
                            // Check if the key is inside the value object
                            // and if it is, assert the value is the same as expected
                            if let Some(value) = category_obj.get(exp_key) {
                                assert_eq!(
                                    exp_v,
                                    value,
                                    "Expected value {0} for key {1} but got {2}",
                                    exp_v,
                                    exp_key,
                                    value.as_str().unwrap_or_default()
                                );
                            };
                        });
                        // Add each column name from this category
                        acc.extend(category_obj.keys().cloned());
                    }
                    acc
                });
        }

        HashSet::new()
    }

    /// Reads the scuba log file and parses all samples as ScubaTelemetryLogSample
    fn deserialize_scuba_log_file(scuba_log_file: &str) -> Result<Vec<ScubaTelemetryLogSample>> {
        use std::fs::File;
        use std::io::BufRead;
        use std::io::BufReader;

        let file = File::open(scuba_log_file)?;
        let reader = BufReader::new(file);

        // Collect all lines first (not efficient for very large files, but works for test logs)
        let lines: Vec<String> = reader.lines().collect::<std::io::Result<_>>()?;

        // Parse each line as a ScubaTelemetryLogSample object
        let mysql_tels: Vec<ScubaTelemetryLogSample> = lines
            .into_iter()
            .map(|line| {
                serde_json::from_str::<serde_json::Value>(&line)
                    .map_err(anyhow::Error::from)
                    .and_then(|json| {
                        // Scuba groups the logs by type (e.g. int, normal), so
                        // let's remove those and flatten the sample into a single
                        // json object.
                        let flattened_log = json
                            .as_object()
                            .iter()
                            .flat_map(|obj| {
                                obj.iter().flat_map(|(_, category_values)| {
                                    category_values.as_object().into_iter().flat_map(
                                        |category_obj| {
                                            category_obj.iter().map(|(k, v)| (k.clone(), v.clone()))
                                        },
                                    )
                                })
                            })
                            .collect::<serde_json::Value>();

                        println!("flattened_log: {flattened_log:#?}");

                        let success: bool = flattened_log["success"]
                            .as_i64()
                            .map(|i| i == 1)
                            .expect("success should always be logged");
                        let granularity = serde_json::from_value::<TelemetryGranularity>(
                            flattened_log["granularity"].clone(),
                        )?;

                        let query_name: Option<String> =
                            flattened_log["query_name"].as_str().map(String::from);

                        let repo_ids: Vec<RepositoryId> = flattened_log["repo_ids"]
                            .as_array()
                            .map(|ids| {
                                ids.iter()
                                    .filter_map(|id| {
                                        id.as_str()
                                            .and_then(|s| s.parse::<i32>().ok())
                                            .map(RepositoryId::new)
                                    })
                                    .sorted()
                                    .collect()
                            })
                            .unwrap_or_default();

                        let transaction_query_names: Vec<String> =
                            flattened_log["transaction_query_names"]
                                .as_array()
                                .map(|ids| {
                                    ids.iter()
                                        .filter_map(|id| id.as_str())
                                        .map(String::from)
                                        .sorted()
                                        .collect()
                                })
                                .unwrap_or_default();
                        // Now deserialize that into a MysqlQueryTelemetry object
                        let mysql_tel =
                            serde_json::from_value::<MysqlQueryTelemetry>(flattened_log)?;

                        Ok(ScubaTelemetryLogSample {
                            mysql_telemetry: mysql_tel,
                            success,
                            repo_ids,
                            granularity,
                            query_name,
                            transaction_query_names,
                        })
                    })
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(mysql_tels)
    }
}

#[allow(
    dead_code,
    unreachable_code,
    unused_variables,
    clippy::diverging_sub_expression,
    clippy::todo
)]
#[ignore]
#[mononoke::fbinit_test]
async fn should_compile(fb: FacebookInit) -> Result<()> {
    let config: &SqlQueryConfig = todo!();
    let connection: &sql::Connection = todo!();

    let cri = ClientRequestInfo::new(ClientEntryPoint::Sapling);
    let client_info = ClientInfo::new()?;
    let mut metadata = Metadata::default();
    metadata.add_client_info(client_info);

    let sql_query_tel = SqlQueryTelemetry::new(fb, metadata);
    TestQuery::query(connection, sql_query_tel, todo!(), todo!()).await?;
    TestQuery::query_with_transaction(todo!(), todo!(), todo!()).await?;
    TestQuery2::query(config, None, connection, sql_query_tel).await?;
    TestQuery2::query(
        config,
        Some(std::time::Duration::from_secs(60)),
        connection,
        sql_query_tel,
    )
    .await?;
    TestQuery2::query_with_transaction(todo!()).await?;
    TestQuery3::query(connection, sql_query_tel, &[(&12,)]).await?;
    TestQuery3::query_with_transaction(todo!(), &[(&12,)]).await?;
    TestQuery4::query(connection, sql_query_tel, &"hello").await?;
    TestQuery::query(connection, sql_query_tel, todo!(), todo!()).await?;
    TestQuery2::query(config, None, connection, sql_query_tel).await?;
    TestQuery3::query(connection, sql_query_tel, &[(&12,)]).await?;
    TestQuery4::query(connection, sql_query_tel, &"hello").await?;
    TestQuery::query(connection, sql_query_tel, todo!(), todo!()).await?;
    TestQuery2::query(config, None, connection, sql_query_tel).await?;
    TestQuery3::query(connection, sql_query_tel, &[(&12,)]).await?;
    TestQuery4::query(connection, sql_query_tel, &"hello").await?;
    Ok(())
}
