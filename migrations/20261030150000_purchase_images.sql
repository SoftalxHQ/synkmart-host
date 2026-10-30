CREATE TABLE IF NOT EXISTS purchase_images (
  id INTEGER PRIMARY KEY NOT NULL,
  purchase_id INTEGER NOT NULL REFERENCES purchases(id) ON DELETE CASCADE,
  filename TEXT NOT NULL,
  content_type TEXT NOT NULL DEFAULT 'image/jpeg',
  created_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE INDEX IF NOT EXISTS index_purchase_images_on_purchase_id ON purchase_images(purchase_id);
