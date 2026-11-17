-- Synthetic fixture for schema v1: one or two rows in every table, so that
-- foreign keys, CHECK constraints and STRICT typing are exercised.
-- All text is invented for the tests (R-95); the source id starts with "test-".

INSERT INTO pack_import (id, pack_id, content_sha256, format_version, source_slug, exported_at,
                         part_index, part_total, via, imported_at, report_json)
VALUES (1, 'test-fixture-book--2026-11-17--1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa', 1,
        'test-fixture-book', '2026-11-17', 1, 1, 'paste', '2026-11-17T10:00:00Z', '{"words":1}');

INSERT INTO source (id, slug, type, title, author, reading_position, reading_position_exported_at, created_at, updated_at)
VALUES (1, 'test-fixture-book', 'book', 'The Invented Lighthouse', 'Test Author', 'chapter 3', '2026-11-17',
        '2026-11-17T10:00:00Z', '2026-11-17T10:00:00Z');

INSERT INTO word (id, headword, headword_key, grammar, meaning, usefulness, goal, note, created_at, updated_at)
VALUES (1, 'glimmerwick', 'glimmerwick', 'noun', 'an invented word for a small flickering lamp', 6, 'recognise', '',
        '2026-11-17T10:00:00Z', '2026-11-17T10:00:00Z');

INSERT INTO word_source (word_id, source_id, seq, tag_kind, tag_text, first_pack_import_id, last_pack_import_id)
VALUES (1, 1, 1, NULL, NULL, 1, 1);

INSERT INTO word_item (word_id, kind, value, value_key, position, origin)
VALUES (1, 'czech', 'lampička', 'lampička', 0, 'pack'),
       (1, 'accepted', 'glimmerwicks', 'glimmerwicks', 0, 'pack');

INSERT INTO word_form (word_id, form, form_key, common, position)
VALUES (1, 'glimmerwicks', 'glimmerwicks', 1, 0);

INSERT INTO sentence (id, word_id, en_marked, cs_marked, en_key, origin, location, source_id, position,
                      pack_import_id, created_at, updated_at)
VALUES (1, 1, 'The [[glimmerwick]] on the shelf flickered twice.', '[[Lampička]] na polici dvakrát zablikala.',
        'the glimmerwick on the shelf flickered twice.', 'example', NULL, 1, 0, 1,
        '2026-11-17T10:00:00Z', '2026-11-17T10:00:00Z');

INSERT INTO word_override (word_id, field, set_at) VALUES (1, 'usefulness', '2026-11-17T11:00:00Z');

INSERT INTO field_conflict (word_id, field, current_value, incoming_value, pack_import_id, status, created_at)
VALUES (1, 'meaning', '"an invented word for a small flickering lamp"', '"a tiny invented lamp"', 1, 'open',
        '2026-11-17T10:00:00Z');

INSERT INTO card (id, word_id, kind, format, state, stability, difficulty, due_day, last_review_day,
                  last_review_at, introduced_day, reps, created_at)
VALUES (1, 1, 'understand', 'sentence', 'review', 3.2, 5.1, '2026-11-20', '2026-11-17', '2026-11-17T10:05:00Z',
        '2026-11-17', 1, '2026-11-17T10:00:00Z');

INSERT INTO practice_session (id, study_day, scope_source_id, block_no, block_size, status, started_at)
VALUES ('00112233445566778899aabbccddeeff', '2026-11-17', 1, 1, 20, 'active', '2026-11-17T10:04:00Z');

INSERT INTO review_log (id, client_review_id, card_id, session_id, reviewed_at, client_ts, review_day, raw_grade,
                        fsrs_rating, suggested_grade, format, sentence_id, answer_match, hint_used, elapsed_days,
                        duration_ms, before_json)
VALUES (1, '6f1e2d3c-0000-4000-8000-000000000001', 1, '00112233445566778899aabbccddeeff', '2026-11-17T10:05:00Z',
        '2026-11-17T10:05:00Z', '2026-11-17', 2, 3, NULL, 'sentence', 1, NULL, 0, 0, 4200, '{"state":"new"}');

INSERT INTO session_item (session_id, ord, card_id, origin, sentence_id, review_id)
VALUES ('00112233445566778899aabbccddeeff', 1.0, 1, 'new', 1, 1);

INSERT INTO fsrs_params (fitted_at, params_json, review_count, log_loss_new, log_loss_old, active)
VALUES ('2026-11-17T04:00:00Z', '[0.2, 1.2]', 400, 0.31, 0.33, 0);

INSERT INTO audio_clip (hash, text, voice, kind, status, created_at, updated_at)
VALUES ('0123456789abcdef0123456789abcdef', 'glimmerwick', 'af_heart', 'word', 'pending',
        '2026-11-17T10:00:00Z', '2026-11-17T10:00:00Z');

INSERT INTO auth_session (token_sha256, password_fingerprint, created_at, last_seen_at, expires_at, user_agent)
VALUES ('bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', 'cccc', '2026-11-17T10:00:00Z',
        '2026-11-17T10:00:00Z', '2027-05-16T10:00:00Z', 'test-agent');

INSERT INTO setting (key, value_json) VALUES ('new_per_day', '5');
