ALTER TABLE products ADD COLUMN status TEXT NOT NULL DEFAULT 'active';

CREATE TABLE IF NOT EXISTS product_variants (
  id INTEGER PRIMARY KEY NOT NULL,
  cloud_id INTEGER,
  product_id INTEGER NOT NULL REFERENCES products(id) ON DELETE CASCADE,
  name TEXT NOT NULL,
  sku TEXT NOT NULL UNIQUE,
  selling_price TEXT NOT NULL DEFAULT '0',
  purchase_price TEXT NOT NULL DEFAULT '0',
  active INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS idx_product_variants_product ON product_variants(product_id);

CREATE TABLE IF NOT EXISTS product_barcodes (
  id INTEGER PRIMARY KEY NOT NULL,
  cloud_id INTEGER,
  product_id INTEGER NOT NULL REFERENCES products(id) ON DELETE CASCADE,
  product_variant_id INTEGER REFERENCES product_variants(id) ON DELETE SET NULL,
  barcode TEXT NOT NULL,
  description TEXT,
  barcode_type TEXT,
  is_primary INTEGER NOT NULL DEFAULT 0,
  active INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT (datetime('now')),
  updated_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_product_barcodes_active
  ON product_barcodes(barcode) WHERE active = 1;
CREATE INDEX IF NOT EXISTS idx_product_barcodes_product ON product_barcodes(product_id);

ALTER TABLE inventory_balances ADD COLUMN product_variant_id INTEGER REFERENCES product_variants(id);
-- SQLite cannot easily replace UNIQUE(product_id, stock_location_id); keep app-level checks for variant rows.
CREATE UNIQUE INDEX IF NOT EXISTS idx_inventory_balances_variant_location
  ON inventory_balances(product_variant_id, stock_location_id) WHERE product_variant_id IS NOT NULL;

ALTER TABLE sale_items ADD COLUMN product_variant_id INTEGER REFERENCES product_variants(id);
ALTER TABLE stock_movements ADD COLUMN product_variant_id INTEGER REFERENCES product_variants(id);

INSERT INTO product_barcodes (product_id, barcode, is_primary, active)
SELECT id, barcode, 1, 1 FROM products
WHERE barcode IS NOT NULL AND TRIM(barcode) != ''
  AND NOT EXISTS (
    SELECT 1 FROM product_barcodes pb WHERE pb.product_id = products.id AND pb.barcode = products.barcode
  );
