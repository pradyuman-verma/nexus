-- Per-item content type, so the pipeline + UI can distinguish articles from
-- videos / tweets / pdfs as new ingestion channels land in v2.
ALTER TABLE items ADD COLUMN IF NOT EXISTS content_type TEXT NOT NULL DEFAULT 'article';
-- values: article | video | tweet | pdf | podcast
