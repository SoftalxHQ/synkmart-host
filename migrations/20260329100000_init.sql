-- SynkMart store host local schema

CREATE TABLE IF NOT EXISTS store_meta (
  key TEXT PRIMARY KEY NOT NULL,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS users (
  id INTEGER PRIMARY KEY NOT NULL,
  cloud_user_id INTEGER,
  name TEXT NOT NULL,
  email TEXT NOT NULL UNIQUE,
  role TEXT NOT NULL DEFAULT 'cashier',
  title TEXT,
  status TEXT NOT NULL DEFAULT 'active',
  password_digest TEXT NOT NULL,
  permissions_json TEXT NOT NULL DEFAULT '[]',
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS sessions (
  token TEXT PRIMARY KEY NOT NULL,
  user_id INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  expires_at TEXT
);

CREATE TABLE IF NOT EXISTS categories (
  id INTEGER PRIMARY KEY NOT NULL,
  cloud_id INTEGER,
  name TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS products (
  id INTEGER PRIMARY KEY NOT NULL,
  cloud_id INTEGER,
  category_id INTEGER REFERENCES categories(id),
  name TEXT NOT NULL,
  sku TEXT NOT NULL UNIQUE,
  barcode TEXT,
  description TEXT,
  purchase_price TEXT NOT NULL DEFAULT '0',
  selling_price TEXT NOT NULL DEFAULT '0',
  reorder_level INTEGER,
  featured INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_products_barcode ON products(barcode);
CREATE INDEX IF NOT EXISTS idx_products_sku ON products(sku);

CREATE TABLE IF NOT EXISTS inventory_balances (
  id INTEGER PRIMARY KEY NOT NULL,
  product_id INTEGER NOT NULL REFERENCES products(id) ON DELETE CASCADE,
  branch_id INTEGER NOT NULL,
  stock_location_id INTEGER NOT NULL,
  on_hand INTEGER NOT NULL DEFAULT 0,
  reserved INTEGER NOT NULL DEFAULT 0,
  UNIQUE(product_id, stock_location_id)
);

CREATE TABLE IF NOT EXISTS register_sessions (
  id INTEGER PRIMARY KEY NOT NULL,
  user_id INTEGER NOT NULL REFERENCES users(id),
  branch_id INTEGER NOT NULL,
  stock_location_id INTEGER,
  status TEXT NOT NULL DEFAULT 'open',
  opening_float TEXT NOT NULL DEFAULT '0.00',
  closing_counted TEXT,
  expected_cash TEXT,
  variance TEXT,
  notes TEXT,
  opened_at TEXT NOT NULL DEFAULT (datetime('now')),
  closed_at TEXT
);

CREATE TABLE IF NOT EXISTS sales (
  id INTEGER PRIMARY KEY NOT NULL,
  receipt_number TEXT NOT NULL UNIQUE,
  user_id INTEGER NOT NULL REFERENCES users(id),
  branch_id INTEGER NOT NULL,
  stock_location_id INTEGER,
  customer_name TEXT,
  cashier_name TEXT NOT NULL,
  subtotal TEXT NOT NULL,
  discount TEXT NOT NULL DEFAULT '0.00',
  tax TEXT NOT NULL DEFAULT '0.00',
  total TEXT NOT NULL,
  change_amount TEXT NOT NULL DEFAULT '0.00',
  status TEXT NOT NULL DEFAULT 'completed',
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS sale_items (
  id INTEGER PRIMARY KEY NOT NULL,
  sale_id INTEGER NOT NULL REFERENCES sales(id) ON DELETE CASCADE,
  product_id INTEGER NOT NULL REFERENCES products(id),
  name TEXT NOT NULL,
  sku TEXT NOT NULL,
  unit_price TEXT NOT NULL,
  quantity INTEGER NOT NULL,
  line_total TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sale_payments (
  id INTEGER PRIMARY KEY NOT NULL,
  sale_id INTEGER NOT NULL REFERENCES sales(id) ON DELETE CASCADE,
  method TEXT NOT NULL,
  amount TEXT NOT NULL,
  tendered TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS stock_movements (
  id INTEGER PRIMARY KEY NOT NULL,
  product_id INTEGER NOT NULL REFERENCES products(id),
  stock_location_id INTEGER NOT NULL,
  branch_id INTEGER NOT NULL,
  user_id INTEGER,
  kind TEXT NOT NULL,
  quantity INTEGER NOT NULL,
  balance_after INTEGER,
  document_type TEXT,
  document_id INTEGER,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS idempotency_keys (
  key TEXT PRIMARY KEY NOT NULL,
  response_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS outbox (
  id INTEGER PRIMARY KEY NOT NULL,
  event_id TEXT NOT NULL UNIQUE,
  event_type TEXT NOT NULL,
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  synced_at TEXT
);

CREATE TABLE IF NOT EXISTS pairing (
  code TEXT PRIMARY KEY NOT NULL,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  expires_at TEXT NOT NULL
);
