-- Branches, locations, and remaining offline store tables

CREATE TABLE IF NOT EXISTS branches (
  id INTEGER PRIMARY KEY NOT NULL,
  cloud_id INTEGER,
  name TEXT NOT NULL,
  slug TEXT NOT NULL,
  city TEXT NOT NULL DEFAULT '',
  status TEXT NOT NULL DEFAULT 'active',
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS stock_locations (
  id INTEGER PRIMARY KEY NOT NULL,
  cloud_id INTEGER,
  branch_id INTEGER NOT NULL REFERENCES branches(id),
  name TEXT NOT NULL,
  kind TEXT NOT NULL DEFAULT 'store',
  slug TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'active',
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS customers (
  id INTEGER PRIMARY KEY NOT NULL,
  cloud_id INTEGER,
  name TEXT NOT NULL,
  phone TEXT,
  email TEXT,
  credit_limit TEXT NOT NULL DEFAULT '0',
  credit_balance TEXT NOT NULL DEFAULT '0',
  notes TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS suppliers (
  id INTEGER PRIMARY KEY NOT NULL,
  cloud_id INTEGER,
  name TEXT NOT NULL,
  phone TEXT,
  email TEXT,
  company TEXT,
  status TEXT NOT NULL DEFAULT 'active',
  balance TEXT NOT NULL DEFAULT '0',
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS purchases (
  id INTEGER PRIMARY KEY NOT NULL,
  cloud_id INTEGER,
  reference_number TEXT NOT NULL UNIQUE,
  supplier_id INTEGER REFERENCES suppliers(id),
  branch_id INTEGER,
  stock_location_id INTEGER,
  status TEXT NOT NULL DEFAULT 'draft',
  notes TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS purchase_items (
  id INTEGER PRIMARY KEY NOT NULL,
  purchase_id INTEGER NOT NULL REFERENCES purchases(id) ON DELETE CASCADE,
  product_id INTEGER NOT NULL REFERENCES products(id),
  name TEXT NOT NULL,
  sku TEXT,
  quantity INTEGER NOT NULL,
  received_quantity INTEGER NOT NULL DEFAULT 0,
  unit_cost TEXT NOT NULL DEFAULT '0'
);

CREATE TABLE IF NOT EXISTS stock_adjustments (
  id INTEGER PRIMARY KEY NOT NULL,
  reference_number TEXT NOT NULL UNIQUE,
  branch_id INTEGER,
  stock_location_id INTEGER,
  user_id INTEGER,
  status TEXT NOT NULL DEFAULT 'pending',
  reason TEXT NOT NULL,
  reject_note TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS stock_adjustment_items (
  id INTEGER PRIMARY KEY NOT NULL,
  adjustment_id INTEGER NOT NULL REFERENCES stock_adjustments(id) ON DELETE CASCADE,
  product_id INTEGER NOT NULL REFERENCES products(id),
  name TEXT NOT NULL,
  action TEXT NOT NULL,
  quantity INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS stock_transfers (
  id INTEGER PRIMARY KEY NOT NULL,
  reference_number TEXT NOT NULL UNIQUE,
  from_branch_id INTEGER,
  to_branch_id INTEGER,
  from_stock_location_id INTEGER NOT NULL,
  to_stock_location_id INTEGER NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  scope TEXT NOT NULL DEFAULT 'internal',
  cancel_reason TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS stock_transfer_items (
  id INTEGER PRIMARY KEY NOT NULL,
  transfer_id INTEGER NOT NULL REFERENCES stock_transfers(id) ON DELETE CASCADE,
  product_id INTEGER NOT NULL REFERENCES products(id),
  name TEXT NOT NULL,
  sku TEXT,
  quantity INTEGER NOT NULL,
  received_quantity INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS activity_logs (
  id INTEGER PRIMARY KEY NOT NULL,
  action TEXT NOT NULL,
  summary TEXT NOT NULL,
  actor TEXT,
  user_id INTEGER,
  branch_id INTEGER,
  metadata_json TEXT,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS sale_returns (
  id INTEGER PRIMARY KEY NOT NULL,
  sale_id INTEGER NOT NULL REFERENCES sales(id),
  user_id INTEGER,
  reason TEXT,
  method TEXT NOT NULL DEFAULT 'cash',
  total TEXT NOT NULL DEFAULT '0',
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS sale_return_items (
  id INTEGER PRIMARY KEY NOT NULL,
  return_id INTEGER NOT NULL REFERENCES sale_returns(id) ON DELETE CASCADE,
  sale_item_id INTEGER NOT NULL,
  product_id INTEGER,
  quantity INTEGER NOT NULL
);
