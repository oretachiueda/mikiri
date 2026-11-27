# Rule test mapping

| r76 test | Rust test or disposition |
| --- | --- |
| `test_ban_0800` | `decide::tests::test_ban_0800` |
| `test_late_entry` | `decide::tests::test_late_entry` |
| `test_spread_guard` | `decide::tests::test_spread_guard` |
| `test_size_limits` | `decide::tests::test_size_limits` |
| `test_size_limits_low_equity` | `decide::tests::test_size_limits_low_equity` |
| `test_idempotency_retry_restart` | `decide::tests::test_idempotency_retry_restart` |
| `test_partial_entry` | `decide::tests::test_partial_entry` |
| `test_exit_residual` | `decide::tests::test_exit_residual` |
| `test_daily_order_cap` | `decide::tests::test_daily_order_cap` |
| `test_kill_switch` | `decide::tests::test_kill_switch` |
| `test_halt_sticky` | `decide::tests::test_halt_sticky` |
| `test_fix_1_late_exit_next_day` | `decide::tests::test_fix_1_late_exit_next_day` |
| `test_fix_2_position_lookup` | `decide::tests::test_fix_2_position_lookup` |
| `test_fix_2_position_without_price_needs_human_once` | `decide::tests::test_fix_2_position_without_price_needs_human_once` |
| `test_holiday_no_event` | `decide::tests::test_holiday_no_event` |
| `test_jst_on_utc_host` | `decide::tests::test_jst_on_utc_host` |
| `test_idempotency_lock` | 5b: process lock belongs to persistent execution. |
| `test_idempotency_timeout` | 5b: recovery and broker lookup belong to execution. |
| `test_no_secrets_in_journal` | 5b: journal output is not part of decisions. |
| `test_calendar_dates (renamed)` | 5c: calendar generation is outside the plan-driven decision. |
| `test_calendar_missing_year` | 5c: calendar coverage belongs to plan preparation. |
| `test_default_env_sim` | 5c: environment selection belongs to execution. |
| `test_default_env_is_sim` | 5c: environment selection belongs to execution. |
| `test_live_url_fails` | 5c: URL restrictions belong to the adapter. |
| `test_live_enabled_false_in_source` | Not applicable (Python only): Python source assertion. |
| `test_live_enabled_false` | Not applicable (Python only): Python module assertion. |
| `test_fix_3_saxo_mapping` | 5c: broker mapping belongs to the adapter. |
| `test_fix_4_dry_run_flag` | 5c: command-line mode belongs to execution. |
| `test_fix_5_calendar_no_pycache` | Not applicable (Python only): Python bytecode cache. |
| `test_fix_6_reject_traversal_and_cancel` | 5c: adapter URL and cancellation validation. |
| `test_fix_7_redirect_refused` | 5c: HTTP redirect policy belongs to the adapter. |

## Stop leg

| r76 test | Rust test or disposition |
| --- | --- |
| `test_t1_entry_window_and_stop_price` | `decide::stop_tests::test_t1_entry_window_and_stop_price` |
| `test_t2_close_at_1100_cancels_stop_then_buys` | `decide::stop_tests::test_t2_close_cancels_stop_then_buys` |
| `test_t3_stop_filled_before_1100_no_buy` | `decide::stop_tests::test_t3_stop_filled_before_close_no_buy` |
| `test_t4_race_stop_fills_during_cancel` | `decide::stop_tests::test_t4_race_stop_fills_during_cancel` |
| `test_t5_stop_rejected_closes_immediately` | `decide::stop_tests::test_t5_stop_rejected_closes_immediately` |
| `test_t6_late_entry_missed` | `decide::stop_tests::test_t6_late_entry_missed` |
| `test_t7_spread_and_size_skips` | `decide::stop_tests::test_t7_spread_and_size_skips` |
| `test_t8_restart_no_duplicate_keys` | `decide::stop_tests::test_t8_restart_no_duplicate_keys` |
| `test_t9_kill_shared_with_i1` | `decide::stop_tests::test_t9_kill_shared` |
| `test_t10_halt_blocks_entry` | `decide::stop_tests::test_t10_halt_blocks_entry` |
| `test_t11_full_day_i1_then_l2s` | 5c: full runtime orchestration. |
| `test_t12_i1_open_at_0956_sizes_with_bid` | `decide::stop_tests::test_t12_other_open_sizes_with_bid` |
| `test_t13_entry_timeout_then_lookup_places_stop` | `decide::stop_tests::test_t13_entry_timeout_then_lookup_places_stop` |
| `test_t14_stop_timeout_no_double_buy` | `decide::stop_tests::test_t14_stop_timeout_no_double_buy` |
| `test_t15_no_network` | 5c: network isolation of execution. |
| `test_t16_live_refused` | 5c: runtime environment gate. |
| `test_t17_shared_halt_after_l2s_loss` | 5b: persistence and reconciliation. |
| `test_grok_b2_1_partial_stop_cancelled` | `decide::stop_tests::test_grok_b2_1_partial_stop_cancelled` |
| `test_grok_b2_2_missing_price_needs_human` | `decide::stop_tests::test_grok_b2_2_missing_price_needs_human` |
| `test_grok_b2_3_rejected_unprotected` | `decide::stop_tests::test_grok_b2_3_rejected_unprotected` |
| `test_grok_b2_4_halt_with_open_i1` | `decide::stop_tests::test_grok_b2_4_halt_with_open_other` |
| `test_grok_b2_5_i1_unresolved_alert` | 5c: cross-leg orchestration alert. |
| `test_grok_b2_6_send_at_0800_refused` | `decide::stop_tests::test_grok_b2_6_send_in_blackout_refused` |
| `test_grok_b2_7_i1_live_refused` | 5c: runtime environment gate. |
| `test_g3_partial_waits_cancel` | `decide::stop_tests::test_g3_partial_waits_cancel` |
| `test_g3_restart_unprotected_short` | `decide::stop_tests::test_g3_restart_unprotected_short` |
| `test_grok_c_1_cancelled_partial_fill_accounted` | `decide::stop_tests::test_grok_c_1_cancelled_partial_fill_accounted` |
| `test_grok_c_2_missing_order_is_unknown` | `decide::stop_tests::test_grok_c_2_missing_order_is_unknown` |
| `test_grok_c_3_direct_step_moving_clock_warning` | 5c: caller clock and orchestration. |
| `test_grok_c_2_unknown_status_on_each_submit` | 5c: broker submission and reconciliation. |
| `test_grok_c_2_filled_stop_hidden_from_open_orders` | 5c: broker order lookup. |
| `test_m1_partial_exit_waits_for_cancel_confirmation` | `decide::stop_tests::test_m1_partial_exit_waits_for_cancel_confirmation` |
| `test_m2_send_checks_fresh_clock` | 5c: clock recheck before sending. |
| `test_m3_kill_cancels_after_cap` | 5c: prior Python kill cancellation; current rule blocks requests. |
| `test_m4_env_is_keyword_only` | Not applicable (Python only): Python constructor signature. |
| `test_m1_partial_exit_confirmed_and_cap` | 5c: execution and cancellation confirmation. |
| `test_m1_cancel_cap_blocks_retry` | `decide::stop_tests::test_m1_cancel_cap_blocks_retry` |
| `test_grok_d_1_late_fill_blocks_next_entry` | `decide::stop_tests::test_grok_d_1_late_fill_blocks_next_entry` |
| `test_grok_d_2_stop_refusal_returns_needs_human` | `decide::stop_tests::test_grok_d_2_stop_refusal_returns_needs_human` |
| `test_grok_d_3_no_stop_cancel_in_0800_window` | `decide::stop_tests::test_grok_d_3_no_stop_cancel_in_blackout` |
| `test_grok_d_1_unchanged_terminal_allows_next_event` | 5b: persistent terminal reconciliation. |
| `test_grok_d_1_late_market_fill_after_restart_blocks` | 5b: persisted fill reconciliation. |
| `test_grok_d_3_close_refusal_after_cancel_alerts` | 5c: send-time refusal. |
| `test_grok_d_1b_normal_intraday_stop_fill_booked` | 5b: fill ledger recording. |
| `test_grok_d_1b_partial_stop_increment_then_complete` | 5b: incremental fill ledger recording. |
| `test_grok_d_1b_partial_stop_then_1100_close` | `decide::stop_tests::test_grok_d_1b_partial_stop_then_close` |
| `test_grok_d_1b_overfill_or_missing_price_needs_human` | `decide::stop_tests::test_grok_d_1b_overfill_or_missing_price_needs_human` |

## Journal replay tests

| Python writer test | Rust test |
| --- | --- |
| `test_size_under_1000_none` | `replay::tests::test_size_under_minimum_none` |
| test_size_cap_5&#48;00 | `replay::tests::test_size_cap` |
| `test_halted_boundary` | `replay::tests::test_halted_boundary` |
| `test_open_mtm_long_short` | `replay::tests::test_open_mtm_long_short` |
| `test_sizing_equity` | `replay::tests::test_sizing_equity` |
| `test_correction_replaces_pnl` | `replay::tests::test_correction_replaces_pnl` |

## Python writer tests

| Python test | Rust test or disposition |
| --- | --- |
| test_env_must_be_live | journal::tests::test_env_must_be_live |
| test_env_required | journal::tests::test_env_required |
| test_idem_mock_sim_forbidden | journal::tests::test_idem_mock_sim_forbidden |
| test_fill_requires_order_id_and_hash | journal::tests::test_fill_requires_order_id_and_hash |
| test_forbidden_hash | journal::tests::test_forbidden_hash |
| test_non_go&#116;obi_trade_fails | journal::tests::test_non_event_entry_fails |
| test_format_check_reason_fails | journal::tests::test_format_check_reason_fails |
| test_duplicate_idem_fill | journal::tests::test_duplicate_idem_fill |
| test_exit_fill_next_day_fill_time_ok_on_go&#116;obi_tokyo_date | journal::tests::test_exit_fill_next_day |
| test_close_requires_realized_pnl | journal::tests::test_close_requires_realized_pnl |
| test_append_only_preserves_prefix | journal::tests::test_append_only_preserves_prefix |
| test_size_under_1000_none | replay::tests::test_size_under_minimum_none |
| test_size_cap_5&#48;00 | replay::tests::test_size_cap |
| test_halted_boundary | replay::tests::test_halted_boundary |
| test_open_mtm_long_short | replay::tests::test_open_mtm_long_short |
| test_sizing_equity | replay::tests::test_sizing_equity |
| test_prod_v1_jsonl_absent | 5c: production paths are supplied by the operator. |
| test_period_and_reference_dates_jan31 | Judgment remains in Python; outside journal replay. |
| test_add_months_clamps_february | Judgment remains in Python; outside journal replay. |
| test_weekend_fill_attribution | Judgment remains in Python; outside journal replay. |
| test_start_and_not_ended | Judgment remains in Python; outside journal replay. |
| test_cost_pending_hold | Judgment remains in Python; outside journal replay. |
| test_swap_pending_not_hold | Judgment remains in Python; outside journal replay. |
| test_swap_does_not_change_sr | Judgment remains in Python; outside journal replay. |
| test_open_position_hold | Judgment remains in Python; outside journal replay. |
| test_correction_replaces_pnl | replay::tests::test_correction_replaces_pnl |
| test_zero_fill_business_days | Judgment remains in Python; outside journal replay. |
| test_k_not_one_rejected | Judgment remains in Python; outside journal replay. |
| test_z_hand_expected | Judgment remains in Python; outside journal replay. |
| test_is_go&#116;obi_sample | 5c: event dates come from the plan. |
| test_grok_d_4_correction_target_must_exist_and_match_key | journal::tests::test_grok_d_4_correction_target_must_exist_and_match_key |
| test_grok_b1_01_uncovered_year_judge_ok | Judgment remains in Python; outside journal replay. |
| test_grok_b1_02_period_end_non_business_pnl | Judgment remains in Python; outside journal replay. |
| test_grok_b1_03_pre_start_checks_all_rows | Judgment remains in Python; outside journal replay. |
| test_grok_b1_04_ref_pending_outside_ref_window | Judgment remains in Python; outside journal replay. |
| test_grok_b1_05_empty_order_id | journal::tests::test_fill_requires_order_id_and_hash |
| test_grok_b1_06_duplicate_scheduled_same_key | journal::tests::test_grok_b1_06_duplicate_scheduled_same_key |
| test_grok_b1_07_format_and_skip_hash_and_declare_fill | journal::tests::test_format_check_reason_fails and test_declare_fill_and_skipped_forbidden_hash |
| test_grok_b1_08_append_under_lock_single_fill | journal::tests::test_grok_b1_08_append_under_lock_single_fill |
| test_grok_b1_09_judgment_capital_non_positive_hold | Judgment remains in Python; outside journal replay. |
| test_grok_b1_10_main_stats_zero_variance_hold | Judgment remains in Python; outside journal replay. |
| test_grok_b1_11_zero_fill_nov26 | Judgment remains in Python; outside journal replay. |
| test_grok_b1_extra_halt_idem_rejected | journal::tests::test_halt_key_rejected |
| test_grok_c_1_duplicate_fill_different_actions | journal::tests::test_grok_c_1_duplicate_fill_different_actions |
| test_grok_c_2_nonpositive_reference_capital | Judgment remains in Python; outside journal replay. |
