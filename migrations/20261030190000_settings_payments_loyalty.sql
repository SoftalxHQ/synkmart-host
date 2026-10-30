CREATE TABLE IF NOT EXISTS payment_methods (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  code TEXT NOT NULL,
  name TEXT NOT NULL,
  active INTEGER NOT NULL DEFAULT 1,
  sort_order INTEGER NOT NULL DEFAULT 0,
  opens_drawer INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now')),
  UNIQUE(code)
);

INSERT OR IGNORE INTO payment_methods(code, name, active, sort_order, opens_drawer)
VALUES
  ('cash', 'Cash', 1, 0, 1),
  ('transfer', 'Transfer', 1, 1, 0),
  ('card', 'Card', 1, 2, 0),
  ('credit', 'Credit', 1, 3, 0);

CREATE TABLE IF NOT EXISTS loyalty_programs (
  id INTEGER PRIMARY KEY CHECK (id = 1),
  enabled INTEGER NOT NULL DEFAULT 0,
  earn_rate REAL NOT NULL DEFAULT 1,
  redeem_rate REAL NOT NULL DEFAULT 100,
  min_redeem_points INTEGER NOT NULL DEFAULT 0,
  settings_json TEXT NOT NULL DEFAULT '{}'
);

INSERT OR IGNORE INTO loyalty_programs(id, enabled, earn_rate, redeem_rate, min_redeem_points)
VALUES (1, 0, 1, 100, 0);

CREATE TABLE IF NOT EXISTS loyalty_accounts (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  customer_id INTEGER NOT NULL UNIQUE,
  points_balance INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE TABLE IF NOT EXISTS loyalty_ledger_entries (
  id INTEGER PRIMARY KEY AUTOINCREMENT,
  loyalty_account_id INTEGER NOT NULL,
  sale_id INTEGER,
  entry_type TEXT NOT NULL,
  points INTEGER NOT NULL,
  note TEXT,
  created_by INTEGER,
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

INSERT OR IGNORE INTO store_meta(key, value) VALUES
  ('settings_json', '{"general":{"timezone":"Africa/Lagos","date_format":"DD/MM/YYYY","time_format":"24h","language":"en","session_timeout_minutes":60,"receipt_footer":""},"sales":{"require_customer":false,"max_discount_percent":100,"return_reason_required":false,"allow_held_sales":true},"stock":{"allow_negative_stock":false,"require_adjustment_reason":true,"reorder_default_days":7},"pos":{"require_register_open":true,"require_register_close_count":false},"loyalty":{"enabled":false,"earn_rate":1,"redeem_rate":100,"min_redeem_points":0}}');
