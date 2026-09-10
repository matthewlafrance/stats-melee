//! Background work and the channels that report it back.
//!
//! `SqliteConnection` is `!Send`, so a worker cannot borrow the app's
//! connection — each one opens its own against the same path and posts a
//! message home. These methods are the spawn side and the poll side of
//! that arrangement, kept together so the message shapes and the code that
//! produces and consumes them stay in view of each other.

use super::*;

impl StatsMeleeApp {
    /// Ensure `self.db_conn` is open against the config's current effective
    /// DB path. Re-opens if the user pointed the config at a new location.
    /// Leaves `self.db_error` populated on failure and clears `db_conn`.
    pub(crate) fn ensure_db(&mut self) {
        let target = match self.config.effective_db_path() {
            Ok(p) => p,
            Err(e) => {
                self.db_error = Some(e.to_string());
                self.db_conn = None;
                return;
            }
        };

        // Already open against the same path — nothing to do.
        if self.db_opened_path.as_deref() == Some(target.as_path()) && self.db_conn.is_some() {
            return;
        }

        match stats_melee::open_database(&target) {
            Ok(conn) => {
                self.db_conn = Some(conn);
                self.db_opened_path = Some(target);
                self.db_error = None;
            }
            Err(e) => {
                self.db_conn = None;
                self.db_error = Some(e.to_string());
            }
        }
    }

    /// Reload the cached row list from the open DB. No-op if the DB isn't
    /// open yet — caller should invoke `ensure_db` first.
    pub(crate) fn reload_rows(&mut self) {
        let Some(conn) = self.db_conn.as_mut() else {
            return;
        };

        let code_filter = {
            let code = self.config.user_player_code.trim();
            if code.is_empty() {
                None
            } else {
                Some(code.to_string())
            }
        };

        match replay_list::load_rows(conn, code_filter.as_deref()) {
            Ok(mut rows) => {
                // Apply the user's current column sort. The DB query
                // already returns rows by `id desc` (newest first) which
                // matches the default IngestedAt desc ordering, but we
                // re-sort unconditionally so an alternate sort_key
                // survives a refresh.
                replay_list::sort_rows(&mut rows, self.sort_key, self.sort_direction);
                self.rows = rows;
                self.rows_error = None;
            }
            Err(e) => {
                self.rows_error = Some(e.to_string());
            }
        }
    }

    /// Load the cached rows once if they're empty and the DB is open. No-op
    /// otherwise. Shared by the Library page and the `update()` pre-pass so
    /// the Analytics game-id set + filter-panel histograms have data even
    /// when the user lands on Analytics without visiting the Library first.
    pub(crate) fn ensure_rows_loaded(&mut self) {
        if self.rows.is_empty() && self.rows_error.is_none() && self.db_conn.is_some() {
            self.reload_rows();
        }
    }

    /// Cache key / worker signature for the shared-filter summaries: the
    /// player code plus every structured filter field (search excluded).
    pub(crate) fn summary_key(&self, code: &str) -> SummaryKey {
        SummaryKey {
            code: code.to_string(),
            character: self.library_character_filter,
            opp_character: self.library_opp_character_filter,
            stage: self.library_stage_filter,
            outcome: self.library_outcome_filter,
            date_from: self.library_date_from.trim().to_string(),
            date_to: self.library_date_to.trim().to_string(),
            added_from: self.library_added_from.trim().to_string(),
            added_to: self.library_added_to.trim().to_string(),
            opponent_tag: self.library_opponent_tag.trim().to_string(),
        }
    }

    /// Populate `self.viewer_state` for the currently-viewed game.
    /// No-op if `viewing_game_id` is `None` or if the DB isn't open.
    pub(crate) fn reload_viewer(&mut self) {
        let Some(gid) = self.viewing_game_id else {
            return;
        };
        self.ensure_db();
        let Some(conn) = self.db_conn.as_mut() else {
            self.viewer_state = Some(Err(self
                .db_error
                .clone()
                .unwrap_or_else(|| "db not open".to_string())));
            return;
        };
        // Pass the user's player code through so `load_viewer` can flip
        // the scrub-bar palette to "you / opponent" when one of the
        // game's slots matches.
        let user_code = self.config.user_player_code.trim().to_string();
        let user_code = if user_code.is_empty() {
            None
        } else {
            Some(user_code)
        };
        match viewer::load_viewer(
            conn,
            gid,
            &mut self.analysis_cache,
            user_code.as_deref(),
        ) {
            Ok(s) => self.viewer_state = Some(Ok(s)),
            Err(e) => self.viewer_state = Some(Err(e.to_string())),
        }
    }

    /// Spawn a background scan of the configured replay folder. Returns
    /// immediately — the worker thread does the file walk + peppi parses
    /// off the UI thread, then sends back the count via [`IngestMsg`].
    /// [`Self::poll_ingest_worker`] drains the channel each frame.
    ///
    /// `diesel::SqliteConnection` is `!Send`, same constraint as the
    /// summary worker — the worker opens its own connection against
    /// the DB path. SQLite is happy with a second concurrent handle.
    pub(crate) fn ingest_replays(&mut self) {
        // Already in flight — don't double-spawn.
        if self.ingest_rx.is_some() {
            return;
        }

        let Some(replay_dir) = self.config.replay_dir.clone() else {
            self.last_ingest_summary = Some("No replay folder configured.".to_string());
            return;
        };

        let db_path = match self.config.effective_db_path() {
            Ok(p) => p,
            Err(e) => {
                self.last_ingest_summary = Some(format!("db path: {e}"));
                return;
            }
        };

        let (tx, rx) = mpsc::channel::<IngestMsg>();
        let ctx_for_thread = self.egui_ctx.clone();

        thread::spawn(move || {
            let msg = match stats_melee::open_database(&db_path) {
                Ok(mut conn) => match stats_melee::parse_new_replays(
                    &mut conn,
                    &replay_dir,
                    &db_path,
                ) {
                    Ok(n) => IngestMsg::Ok(n),
                    Err(e) => IngestMsg::Err(e.to_string()),
                },
                Err(e) => IngestMsg::Err(e.to_string()),
            };
            // Best-effort send + nudge eframe to repaint so the
            // status flips immediately rather than after a mouse
            // move.
            let _ = tx.send(msg);
            if let Some(ctx) = ctx_for_thread {
                ctx.request_repaint();
            }
        });

        self.ingest_rx = Some(rx);
        self.ingest_loading = true;
        self.last_ingest_summary = Some("Scanning replays…".to_string());
    }

    /// Drain any pending message from the ingest worker. Called at
    /// the top of every `update()` so the result lands on the frame
    /// it arrives. Same shape as [`Self::poll_summary_worker`].
    pub(crate) fn poll_ingest_worker(&mut self) {
        let Some(rx) = self.ingest_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(IngestMsg::Ok(n)) => {
                self.last_ingest_summary = Some(format!("Ingested {n} new replay(s)."));
                self.ingest_rx = None;
                self.ingest_loading = false;
                // Refresh the visible rows now that the DB is up
                // to date. Cheap — just re-runs the existing
                // load_rows query.
                self.reload_rows();
            }
            Ok(IngestMsg::Err(e)) => {
                self.last_ingest_summary = Some(format!("Ingest failed: {e}"));
                self.ingest_rx = None;
                self.ingest_loading = false;
            }
            Err(mpsc::TryRecvError::Empty) => {
                // Still scanning — leave state untouched.
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                self.last_ingest_summary =
                    Some("Ingest worker exited without producing a result.".to_string());
                self.ingest_rx = None;
                self.ingest_loading = false;
            }
        }
    }

    /// Kick the summary worker if the cached summaries are stale for the
    /// current code + shared filter. Shared by the Analytics and Career
    /// pages — both read from the same `(filtered_summary, career_summary,
    /// win_analytics)` triple computed in one worker pass.
    pub(crate) fn ensure_summary_loaded(&mut self, code: &str) {
        let target_key = self.summary_key(code);
        let needs_load = self.summary_error.is_none()
            && self.db_conn.is_some()
            && !self.summary_loading
            && self.summary_for.as_ref() != Some(&target_key);
        if needs_load {
            self.reload_summary();
        }
    }

    /// Kick off a background recompute of the summaries for the current code
    /// and shared library filter. One worker pass computes three things that
    /// land on the same frame: the filtered summary (Analytics — restricted
    /// to the games the library filter is showing), the whole-career summary,
    /// and the career win-rate breakdowns (both for the Career page). Results
    /// land back via [`poll_summary_worker`].
    ///
    /// `diesel::SqliteConnection` is `!Send`, so we can't hand the one owned
    /// by `self` to the worker. We give the worker its own path and let it
    /// open a second connection — SQLite is happy with a second handle. The
    /// full multi-dimensional library filter is threaded down as an explicit
    /// game-id set (computed here from the in-memory rows), so the filtered
    /// summary reflects exactly the filtered library view.
    pub(crate) fn reload_summary(&mut self) {
        let code = self.config.user_player_code.trim().to_string();
        if code.is_empty() {
            self.filtered_summary = None;
            self.career_summary = None;
            self.win_analytics = None;
            self.summary_error = None;
            self.summary_for = None;
            self.summary_rx = None;
            self.summary_loading = false;
            return;
        }

        let key = self.summary_key(&code);

        let db_path = match self.config.effective_db_path() {
            Ok(p) => p,
            Err(e) => {
                self.filtered_summary = None;
                self.career_summary = None;
                self.win_analytics = None;
                self.summary_error = Some(e.to_string());
                self.summary_for = Some(key);
                return;
            }
        };

        // Build the game-id restriction from the in-memory rows. Only pass it
        // when a structured filter is active — the whole-career case stays a
        // plain unfiltered aggregate (and avoids a needlessly huge IN-list).
        let game_ids = if self.structured_filter_active() {
            Some(self.library_filtered_game_ids())
        } else {
            None
        };
        let filter = PlayerSummaryFilter {
            character_id: None,
            stage_id: None,
            game_ids,
        };

        // The career bundle (filter-independent) only needs recomputing when
        // the code or underlying data changed — not on a filter-only tweak.
        // `summary_for` holds the *previous* key; a differing code (or any of
        // the explicit cache resets, which null these out) forces a refresh.
        let recompute_career = self.career_summary.is_none()
            || self.win_analytics.is_none()
            || self.summary_for.as_ref().map(|k| k.code.as_str()) != Some(code.as_str());

        let (tx, rx) = mpsc::channel::<SummaryMsg>();
        let ctx_for_thread = self.egui_ctx.clone();
        let code_for_thread = code.clone();

        thread::spawn(move || {
            let msg = match stats_melee::open_database(&db_path) {
                Ok(mut conn) => {
                    let filtered = stats_melee::player_summary_filtered(
                        &mut conn,
                        &code_for_thread,
                        &filter,
                    );
                    let career = if recompute_career {
                        let c = stats_melee::player_summary_filtered(
                            &mut conn,
                            &code_for_thread,
                            &PlayerSummaryFilter::NONE,
                        );
                        let a = stats_melee::win_analytics(&mut conn, &code_for_thread);
                        match (c, a) {
                            (Ok(c), Ok(a)) => Ok(Some((c, a))),
                            (Err(e), _) | (_, Err(e)) => Err(e),
                        }
                    } else {
                        Ok(None)
                    };
                    match (filtered, career) {
                        (Ok(f), Ok(bundle)) => SummaryMsg::Ok(f, bundle),
                        (Err(e), _) | (_, Err(e)) => SummaryMsg::Err(e.to_string()),
                    }
                }
                Err(e) => SummaryMsg::Err(e.to_string()),
            };
            // Best-effort send; if the receiver is gone the user already
            // navigated away / triggered another reload, and we just drop.
            let _ = tx.send(msg);
            // Nudge eframe to repaint so the result appears without the
            // user having to wiggle the mouse.
            if let Some(ctx) = ctx_for_thread {
                ctx.request_repaint();
            }
        });

        self.summary_rx = Some(rx);
        self.summary_loading = true;
        self.summary_for = Some(key);
        self.summary_error = None;
    }

    /// Drain any pending message from the summary worker. Called at the
    /// top of every `update()` so results show up the frame they arrive.
    pub(crate) fn poll_summary_worker(&mut self) {
        let Some(rx) = self.summary_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(SummaryMsg::Ok(filtered, career_bundle)) => {
                self.filtered_summary = Some(filtered);
                // A filter-only change carries `None` — keep the existing
                // career data rather than clearing it.
                if let Some((career, a)) = career_bundle {
                    self.career_summary = Some(career);
                    self.win_analytics = Some(a);
                }
                self.summary_error = None;
                self.summary_loading = false;
                self.summary_rx = None;
            }
            Ok(SummaryMsg::Err(e)) => {
                self.filtered_summary = None;
                self.career_summary = None;
                self.win_analytics = None;
                self.summary_error = Some(e);
                self.summary_loading = false;
                self.summary_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {
                // Still computing — leave state untouched.
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                // Worker crashed without sending. Surface something rather
                // than spinning forever.
                self.summary_error =
                    Some("summary worker exited without producing a result".to_string());
                self.summary_loading = false;
                self.summary_rx = None;
            }
        }
    }
}
