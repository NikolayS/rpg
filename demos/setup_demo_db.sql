-- setup_demo_db.sql — create and populate the demo_saas database
-- Used by rpg demo recordings (see demos/README.md).
--
-- Usage:
--   createdb demo_saas
--   psql -d demo_saas -f demos/setup_demo_db.sql
--
-- Copyright 2026 Postgres.ai

-- ------------------------------------------------------------
-- customers
-- ------------------------------------------------------------

comment on database demo_saas is
    'Demo SaaS database used for rpg terminal recordings.';

create table if not exists customers (
    id              int8 generated always as identity primary key,
    name            text        not null,
    email           text        not null unique,
    plan            text        not null default 'free',  -- free | starter | pro | enterprise
    org_id          int8,
    signed_up_at    timestamptz not null default now()
);

comment on table  customers              is 'SaaS customers (one row per account).';
comment on column customers.id           is 'Surrogate primary key.';
comment on column customers.name         is 'Full display name of the customer.';
comment on column customers.email        is 'Unique billing / login email.';
comment on column customers.plan         is 'Subscription tier: free, starter, pro, or enterprise.';
comment on column customers.org_id       is 'Optional organisation grouping id.';
comment on column customers.signed_up_at is 'Timestamp when the account was created.';

insert into customers (name, email, plan, org_id, signed_up_at)
select
    'Customer ' || n                                               as name,
    'customer' || n || '@example.com'                             as email,
    (array['free','starter','pro','enterprise'])[(n % 4) + 1]     as plan,
    nullif((n % 500), 0)                                          as org_id,
    now() - (random() * interval '730 days')                      as signed_up_at
from generate_series(1, 10000) as gs(n);

-- ------------------------------------------------------------
-- users
-- ------------------------------------------------------------

create table if not exists users (
    id          int8 generated always as identity primary key,
    email       text        not null unique,
    name        text        not null,
    role        text        not null default 'member',  -- admin | member | viewer
    created_at  timestamptz not null default now()
);

comment on table  users            is 'Application users (login accounts, may differ from customers).';
comment on column users.id         is 'Surrogate primary key.';
comment on column users.email      is 'Unique login email.';
comment on column users.name       is 'Display name.';
comment on column users.role       is 'Access role: admin, member, or viewer.';
comment on column users.created_at is 'Account creation timestamp.';

insert into users (email, name, role, created_at)
select
    'user' || n || '@example.com'                               as email,
    'User ' || n                                                as name,
    (array['admin','member','viewer'])[(n % 3) + 1]            as role,
    now() - (random() * interval '730 days')                   as created_at
from generate_series(1, 5000) as gs(n);

-- ------------------------------------------------------------
-- orders
-- ------------------------------------------------------------

create table if not exists orders (
    id          int8 generated always as identity primary key,
    customer_id int8        not null references customers(id),
    status      text        not null default 'pending',  -- pending | processing | completed | cancelled
    total_cents int8        not null check (total_cents >= 0),
    created_at  timestamptz not null default now(),
    notes       text
);

comment on table  orders             is 'Customer orders.';
comment on column orders.id          is 'Surrogate primary key.';
comment on column orders.customer_id is 'FK to customers.id.';
comment on column orders.status      is 'Order lifecycle state: pending, processing, completed, or cancelled.';
comment on column orders.total_cents is 'Order total stored as integer cents (USD).';
comment on column orders.created_at  is 'When the order was placed.';
comment on column orders.notes       is 'Optional free-text notes.';

insert into orders (customer_id, status, total_cents, created_at, notes)
select
    (random() * 9999 + 1)::int8                                          as customer_id,
    (array['pending','processing','completed','cancelled'])[(n % 4) + 1] as status,
    (random() * 49900 + 100)::int8                                       as total_cents,
    now() - (random() * interval '365 days')                             as created_at,
    null                                                                 as notes
from generate_series(1, 50000) as gs(n);

-- ------------------------------------------------------------
-- invoices
-- ------------------------------------------------------------

create table if not exists invoices (
    id             int8 generated always as identity primary key,
    customer_id    int8        not null references customers(id),
    total_cents    int8        not null check (total_cents >= 0),
    billing_period tstzrange   not null,
    created_at     timestamptz not null default now()
);

comment on table  invoices                is 'Monthly invoices issued to customers.';
comment on column invoices.id             is 'Surrogate primary key.';
comment on column invoices.customer_id    is 'FK to customers.id.';
comment on column invoices.total_cents    is 'Invoice total in integer cents (USD).';
comment on column invoices.billing_period is 'Closed-open range [start, end) for the billing cycle.';
comment on column invoices.created_at     is 'Invoice generation timestamp.';

insert into invoices (customer_id, total_cents, billing_period, created_at)
select
    (random() * 9999 + 1)::int8                                    as customer_id,
    (random() * 49900 + 100)::int8                                 as total_cents,
    tstzrange(
        date_trunc('month', now() - (n % 24 || ' months')::interval),
        date_trunc('month', now() - (n % 24 || ' months')::interval)
            + interval '1 month'
    )                                                              as billing_period,
    date_trunc('month', now() - (n % 24 || ' months')::interval)
        + interval '4 days'                                        as created_at
from generate_series(1, 50000) as gs(n);

-- ------------------------------------------------------------
-- invoice_line_items
-- ------------------------------------------------------------

create table if not exists invoice_line_items (
    id          int8 generated always as identity primary key,
    invoice_id  int8        not null references invoices(id),
    description text        not null,
    amount_cents int8       not null check (amount_cents >= 0)
);

comment on table  invoice_line_items              is 'Individual line items that make up an invoice.';
comment on column invoice_line_items.id           is 'Surrogate primary key.';
comment on column invoice_line_items.invoice_id   is 'FK to invoices.id.';
comment on column invoice_line_items.description  is 'Human-readable description of the charge.';
comment on column invoice_line_items.amount_cents is 'Line item amount in integer cents (USD).';

-- Normal line items: 1-4 per invoice, sum matches invoice total exactly.
-- We insert them in two passes so we can control the total.
insert into invoice_line_items (invoice_id, description, amount_cents)
select
    i.id                                                         as invoice_id,
    (array[
        'Platform subscription',
        'Seat licence',
        'Overage charges',
        'Add-on: advanced analytics',
        'Add-on: SSO'
    ])[((i.id + gs.n) % 5) + 1]                                 as description,
    -- distribute total evenly, last item absorbs the remainder
    case
        when gs.n < 3 then (i.total_cents / 3)
        else i.total_cents - 2 * (i.total_cents / 3)
    end                                                          as amount_cents
from invoices as i
cross join generate_series(1, 3) as gs(n)
where i.id <= 49975;  -- leave the last 25 invoices for the rounding-bug batch

-- ------------------------------------------------------------
-- 25 invoices with rounding bug (total != sum of line items)
-- These are the rows that /ask can find in the demo.
-- ------------------------------------------------------------

-- Insert line items for invoices 49976–50000 where total_cents is set to
-- total_cents + 2 (a deliberate off-by-two rounding bug).
insert into invoice_line_items (invoice_id, description, amount_cents)
select
    i.id                                                         as invoice_id,
    (array['Platform subscription','Seat licence','Overage charges'])[gs.n] as description,
    case
        when gs.n < 3 then (i.total_cents / 3)
        -- subtract 2 from last item → total_cents will be 2 cents more than sum
        else i.total_cents - 2 * (i.total_cents / 3) - 2
    end                                                          as amount_cents
from invoices as i
cross join generate_series(1, 3) as gs(n)
where i.id > 49975;

-- Verify the bug is present (should return 25 rows):
-- select count(*)
-- from invoices as i
-- left join invoice_line_items as ili on ili.invoice_id = i.id
-- group by i.id
-- having i.total_cents <> coalesce(sum(ili.amount_cents), 0);

-- ------------------------------------------------------------
-- basic indexes for demo performance
-- ------------------------------------------------------------

create index if not exists orders_customer_id_idx
    on orders (customer_id);

create index if not exists orders_created_at_idx
    on orders (created_at desc);

create index if not exists invoices_customer_id_idx
    on invoices (customer_id);

create index if not exists invoice_line_items_invoice_id_idx
    on invoice_line_items (invoice_id);

-- NOTE: do NOT create orders_status_created_at_idx here.
-- gif1_optimize demonstrates /optimize suggesting that index.
-- The tape creates it during recording.  Drop it before re-rendering:
--   drop index concurrently if exists orders_status_created_at_idx;

analyze customers, users, orders, invoices, invoice_line_items;
