-- Test schema for Samo integration tests.
-- Uses lowercase keywords per project SQL style guide.

-- ---------------------------------------------------------------------------
-- Tables
-- ---------------------------------------------------------------------------

create table if not exists users (
    id         int8 generated always as identity primary key,
    name       text        not null,
    email      text        not null unique,
    created_at timestamptz not null default now()
);

comment on table  users              is 'Application users';
comment on column users.id          is 'Surrogate primary key';
comment on column users.name        is 'Display name';
comment on column users.email       is 'Unique email address';
comment on column users.created_at  is 'Row creation timestamp';

create table if not exists products (
    id     int8    generated always as identity primary key,
    name   text    not null,
    price  numeric not null check (price >= 0),
    active bool    not null default true
);

comment on table  products          is 'Product catalogue';
comment on column products.id       is 'Surrogate primary key';
comment on column products.name     is 'Product name';
comment on column products.price    is 'Unit price';
comment on column products.active   is 'Whether the product is listed';

create table if not exists orders (
    id         int8        generated always as identity primary key,
    user_id    int8        not null references users (id),
    amount     numeric     not null check (amount >= 0),
    status     text        not null default 'pending'
                           check (status in ('pending', 'paid', 'shipped', 'cancelled')),
    created_at timestamptz not null default now()
);

comment on table  orders            is 'Customer orders';
comment on column orders.id         is 'Surrogate primary key';
comment on column orders.user_id    is 'FK → users.id';
comment on column orders.amount     is 'Order total';
comment on column orders.status     is 'Order lifecycle state';
comment on column orders.created_at is 'Row creation timestamp';

-- ---------------------------------------------------------------------------
-- Indexes
-- ---------------------------------------------------------------------------

create index if not exists orders_user_id_idx
    on orders (user_id);

create index if not exists orders_status_idx
    on orders (status);

create index if not exists orders_created_at_idx
    on orders (created_at desc);

create index if not exists products_active_idx
    on products (active)
    where active = true;

-- ---------------------------------------------------------------------------
-- Seed data — users
-- ---------------------------------------------------------------------------

insert into users (name, email, created_at) values
    ('Alice Smith',   'alice@example.com',   '2024-01-10 09:00:00+00'),
    ('Bob Jones',     'bob@example.com',     '2024-01-11 10:30:00+00'),
    ('Carol White',   'carol@example.com',   '2024-01-12 11:00:00+00'),
    ('Dave Brown',    'dave@example.com',    '2024-02-01 08:00:00+00'),
    ('Eve Davis',     'eve@example.com',     '2024-02-14 12:00:00+00'),
    ('Frank Miller',  'frank@example.com',   '2024-02-20 15:00:00+00'),
    ('Grace Wilson',  'grace@example.com',   '2024-03-01 07:00:00+00'),
    ('Hank Moore',    'hank@example.com',    '2024-03-05 09:30:00+00'),
    ('Iris Taylor',   'iris@example.com',    '2024-03-10 14:00:00+00'),
    ('Jack Anderson', 'jack@example.com',    '2024-03-15 16:00:00+00')
on conflict do nothing;

-- ---------------------------------------------------------------------------
-- Seed data — products
-- ---------------------------------------------------------------------------

insert into products (name, price, active) values
    ('Widget A',    9.99,  true),
    ('Widget B',   14.99,  true),
    ('Gadget Pro',  99.99,  true),
    ('Gadget Lite', 49.99,  true),
    ('Doohickey',    4.99,  true),
    ('Thingamajig',  7.49,  false),
    ('Gizmo',       24.99,  true),
    ('Contraption', 199.99, true),
    ('Doodad',       2.99,  false),
    ('Whatchamacallit', 34.99, true)
on conflict do nothing;

-- ---------------------------------------------------------------------------
-- Sequences (used by \ds integration tests)
-- ---------------------------------------------------------------------------

create sequence if not exists order_ref_seq
    start 1000
    increment 1
    no minvalue
    no maxvalue
    cache 1;

comment on sequence order_ref_seq is 'Human-readable order reference number';

-- ---------------------------------------------------------------------------
-- Views (used by \sv integration tests)
-- ---------------------------------------------------------------------------

create or replace view active_products as
    select
        id,
        name,
        price
    from products
    where active = true;

comment on view active_products is 'Products currently listed for sale';

-- ---------------------------------------------------------------------------
-- Materialized views (used by \dm integration tests)
-- ---------------------------------------------------------------------------

create materialized view if not exists user_order_summary as
    select
        u.id          as user_id,
        u.name        as user_name,
        count(o.id)   as order_count,
        sum(o.amount) as total_amount
    from users as u
    left join orders as o
        on o.user_id = u.id
    group by
        u.id,
        u.name
    with no data;

comment on materialized view user_order_summary
    is 'Aggregated order totals per user (refresh manually)';

-- ---------------------------------------------------------------------------
-- Functions (used by \sf integration tests)
-- ---------------------------------------------------------------------------

create or replace function user_order_count(p_user_id int8)
returns int8
language sql
stable
as $$
    select count(*) from orders where user_id = p_user_id;
$$;

comment on function user_order_count(int8) is 'Returns the number of orders for a user';

-- ---------------------------------------------------------------------------
-- Seed data — orders
-- ---------------------------------------------------------------------------

insert into orders (user_id, amount, status, created_at)
select
    u.id as user_id,
    o.amount,
    o.status,
    o.created_at
from (
    values
        (1,  19.98,  'shipped',   '2024-02-01 10:00:00+00'::timestamptz),
        (1,  99.99,  'paid',      '2024-02-15 11:00:00+00'),
        (2,   9.99,  'pending',   '2024-03-01 09:00:00+00'),
        (2,  49.99,  'shipped',   '2024-03-02 10:00:00+00'),
        (3, 199.99,  'paid',      '2024-03-05 14:00:00+00'),
        (4,   4.99,  'cancelled', '2024-03-06 08:00:00+00'),
        (5,  34.99,  'shipped',   '2024-03-07 09:00:00+00'),
        (6,  14.99,  'pending',   '2024-03-08 10:00:00+00'),
        (7,  24.99,  'paid',      '2024-03-09 11:00:00+00'),
        (8,   7.49,  'shipped',   '2024-03-10 12:00:00+00'),
        (9,  99.99,  'paid',      '2024-03-11 13:00:00+00'),
        (10, 19.98,  'pending',   '2024-03-12 14:00:00+00')
) as o (user_id, amount, status, created_at)
join users as u on u.id = o.user_id
on conflict do nothing;
