// Copyright 2026 Rpg contributors
//
// Benchmarks for key non-AI paths in rpg, comparing throughput of
// pure-Rust routines against equivalent workloads.
//
// Run with:
//   cargo bench --bench non_ai_paths
//
// Results are written to target/criterion/non_ai_paths/*/
use criterion::{black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion};

// ---------------------------------------------------------------------------
// Inline the modules under test via #[path] so this bench binary can access
// them without restructuring the main binary's module tree.
// ---------------------------------------------------------------------------

#[path = "../src/query.rs"]
mod query;

#[path = "../src/pattern.rs"]
mod pattern;

// output.rs uses `crate::query` — that resolves to the `query` module above
// because this bench binary IS the crate.
#[path = "../src/output.rs"]
mod output;

// highlight.rs is standalone.
#[path = "../src/highlight.rs"]
mod highlight;

// ---------------------------------------------------------------------------
// Shared test data
// ---------------------------------------------------------------------------

/// A realistic multi-statement SQL script used to benchmark `split_statements`.
const SQL_MULTI: &str = r#"
select
    u.id as user_id,
    u.email as email,
    count(o.id) as order_count
from users as u
left join orders as o
    on o.user_id = u.id
where
    u.created_at > '2024-01-01T00:00:00'
    and u.active = true
group by
    u.id,
    u.email
order by order_count desc;

insert into audit_log (event, payload, created_at)
values ('user_query', '{"source": "repl"}', now());

update users
set last_seen_at = now()
where id = $1;
"#;

/// A SQL string containing dollar-quoting (heavier path for `split_statements`).
const SQL_DOLLAR_QUOTED: &str = r#"
create or replace function count_active_users()
returns bigint
language sql
as $$
    select count(*)
    from users
    where active = true;
$$;
select count_active_users();
"#;

/// A long SQL query used for tokenization benchmarks.
const SQL_LONG_QUERY: &str = r#"
select
    t.client_id as client_id,
    date(t.created_at) as day,
    sum(t.amount_cents) as total_cents,
    avg(t.amount_cents) as avg_cents,
    count(*) as tx_count,
    u.email as user_email
from telemetry as t
inner join users as u
    on t.user_id = u.id
left join sessions as s
    on s.user_id = u.id
    and s.started_at between t.created_at - interval '1 hour'
                         and t.created_at + interval '1 hour'
where
    t.submission_date > '2024-01-01T00:00:00'
    and t.sample_id = '10'
    and u.active = true
    and (u.role = 'admin' or u.role = 'analyst')
group by
    t.client_id,
    day,
    u.email
having count(*) > 5
order by
    total_cents desc,
    day asc
limit 1000;
"#;

// ---------------------------------------------------------------------------
// Benchmark: metacommand parsing
// ---------------------------------------------------------------------------

/// Representative set of metacommands covering the main parser dispatch paths.
static META_INPUTS: &[&str] = &[
    r"\dt public.*",
    r"\d users",
    r"\l",
    r"\conninfo",
    r"\timing on",
    r"\x auto",
    r"\set VERBOSITY verbose",
    r"\pset format csv",
    r"\df pg_catalog.*",
    r"\dv public.v_*",
    r"\di *_idx",
    r"\watch 5",
    r"\g /tmp/out.txt",
    r"\gset prefix_",
    r"\q",
    r"\?",
];

fn bench_metacmd_parse(c: &mut Criterion) {
    #[path = "../src/metacmd.rs"]
    mod metacmd;

    let mut group = c.benchmark_group("metacmd_parse");

    // Individual command families.
    for input in META_INPUTS {
        group.bench_with_input(
            BenchmarkId::from_parameter(input.trim_start_matches('\\')),
            input,
            |b, i| b.iter(|| metacmd::parse(black_box(i))),
        );
    }

    // Batch: parse all inputs in one iteration (measures aggregate throughput).
    group.bench_function("all_commands", |b| {
        b.iter(|| {
            for input in META_INPUTS {
                black_box(metacmd::parse(black_box(input)));
            }
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: SQL statement splitting
// ---------------------------------------------------------------------------

fn bench_split_statements(c: &mut Criterion) {
    let mut group = c.benchmark_group("split_statements");

    group.bench_function("simple_three_stmts", |b| {
        b.iter(|| query::split_statements(black_box("select 1; select 2; select 3")));
    });

    group.bench_function("multi_stmt_script", |b| {
        b.iter(|| query::split_statements(black_box(SQL_MULTI)));
    });

    group.bench_function("dollar_quoted", |b| {
        b.iter(|| query::split_statements(black_box(SQL_DOLLAR_QUOTED)));
    });

    group.bench_function("single_long_query", |b| {
        b.iter(|| query::split_statements(black_box(SQL_LONG_QUERY)));
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: pattern utilities (to_like, to_regex, where_clause)
// ---------------------------------------------------------------------------

fn bench_pattern(c: &mut Criterion) {
    let mut group = c.benchmark_group("pattern");

    group.bench_function("to_like_no_wildcards", |b| {
        b.iter(|| pattern::to_like(black_box("my_table")));
    });

    group.bench_function("to_like_wildcards", |b| {
        b.iter(|| pattern::to_like(black_box("pub*._t?")));
    });

    group.bench_function("to_regex_plain", |b| {
        b.iter(|| pattern::to_regex(black_box("integer")));
    });

    group.bench_function("to_regex_wildcards", |b| {
        b.iter(|| pattern::to_regex(black_box("pg_catalog.int*")));
    });

    group.bench_function("where_clause_exact", |b| {
        b.iter(|| {
            pattern::where_clause(
                black_box(Some("users")),
                black_box("relname"),
                black_box(Some("nspname")),
            )
        });
    });

    group.bench_function("where_clause_schema_wildcard", |b| {
        b.iter(|| {
            pattern::where_clause(
                black_box(Some("public.*")),
                black_box("relname"),
                black_box(Some("nspname")),
            )
        });
    });

    group.bench_function("where_clause_double_wildcard", |b| {
        b.iter(|| {
            pattern::where_clause(
                black_box(Some("*orders*")),
                black_box("relname"),
                black_box(Some("nspname")),
            )
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: output formatting
// ---------------------------------------------------------------------------

/// Build a synthetic RowSet with `n_rows` rows and `n_cols` text columns.
fn make_rowset(n_rows: usize, n_cols: usize) -> query::RowSet {
    let columns: Vec<query::ColumnMeta> = (0..n_cols)
        .map(|i| query::ColumnMeta {
            name: format!("column_{i}"),
            is_numeric: i % 3 == 0, // every 3rd column is numeric (right-aligned)
        })
        .collect();

    let rows: Vec<Vec<Option<String>>> = (0..n_rows)
        .map(|row| {
            (0..n_cols)
                .map(|col| {
                    if col % 3 == 0 {
                        Some(format!("{}", row * n_cols + col))
                    } else {
                        Some(format!("value_r{row}_c{col}"))
                    }
                })
                .collect()
        })
        .collect();

    query::RowSet { columns, rows }
}

fn bench_format_aligned(c: &mut Criterion) {
    let mut group = c.benchmark_group("format_aligned");

    let cases: &[(usize, usize)] = &[(10, 3), (100, 5), (1000, 8), (10, 10)];

    for &(n_rows, n_cols) in cases {
        let rs = make_rowset(n_rows, n_cols);
        let cfg = output::OutputConfig::default();

        group.bench_with_input(
            BenchmarkId::new("rows_x_cols", format!("{n_rows}x{n_cols}")),
            &(rs, cfg),
            |b, (rs, cfg)| {
                b.iter_batched(
                    || String::with_capacity(n_rows * n_cols * 20),
                    |mut out| {
                        output::format_aligned(&mut out, black_box(rs), black_box(cfg));
                        black_box(out)
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }

    group.finish();
}

fn bench_format_rowset_pset(c: &mut Criterion) {
    let mut group = c.benchmark_group("format_rowset_pset");

    let rs_small = make_rowset(10, 3);
    let rs_large = make_rowset(500, 6);

    let pset_aligned = output::PsetConfig::default();
    let pset_csv = output::PsetConfig {
        format: output::OutputFormat::Csv,
        ..output::PsetConfig::default()
    };
    let pset_json = output::PsetConfig {
        format: output::OutputFormat::Json,
        ..output::PsetConfig::default()
    };

    group.bench_function("aligned_10x3", |b| {
        b.iter_batched(
            || String::with_capacity(1024),
            |mut out| {
                output::format_rowset_pset(&mut out, black_box(&rs_small), &pset_aligned);
                black_box(out)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("aligned_500x6", |b| {
        b.iter_batched(
            || String::with_capacity(65536),
            |mut out| {
                output::format_rowset_pset(&mut out, black_box(&rs_large), &pset_aligned);
                black_box(out)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("csv_500x6", |b| {
        b.iter_batched(
            || String::with_capacity(65536),
            |mut out| {
                output::format_rowset_pset(&mut out, black_box(&rs_large), &pset_csv);
                black_box(out)
            },
            BatchSize::SmallInput,
        );
    });

    group.bench_function("json_500x6", |b| {
        b.iter_batched(
            || String::with_capacity(65536),
            |mut out| {
                output::format_rowset_pset(&mut out, black_box(&rs_large), &pset_json);
                black_box(out)
            },
            BatchSize::SmallInput,
        );
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Benchmark: SQL tokenizer (syntax highlighting)
// ---------------------------------------------------------------------------

fn bench_tokenize(c: &mut Criterion) {
    let mut group = c.benchmark_group("tokenize");

    group.bench_function("short_select", |b| {
        b.iter(|| highlight::tokenize(black_box("select id, name from users where id = 42")));
    });

    group.bench_function("long_query", |b| {
        b.iter(|| highlight::tokenize(black_box(SQL_LONG_QUERY)));
    });

    group.bench_function("metacmd", |b| {
        b.iter(|| highlight::tokenize(black_box(r"\dt public.*")));
    });

    group.bench_function("with_string_literal", |b| {
        b.iter(|| {
            highlight::tokenize(black_box(
                "select * from users where email = 'alice@example.com'",
            ))
        });
    });

    group.bench_function("with_block_comment", |b| {
        b.iter(|| {
            highlight::tokenize(black_box(
                "/* find heavy hitters */ select id, sum(amount) from orders group by id",
            ))
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Criterion harness registration
// ---------------------------------------------------------------------------

criterion_group!(
    benches,
    bench_metacmd_parse,
    bench_split_statements,
    bench_pattern,
    bench_format_aligned,
    bench_format_rowset_pset,
    bench_tokenize,
);
criterion_main!(benches);
