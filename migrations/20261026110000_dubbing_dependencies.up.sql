CREATE TABLE speech.dubbing_dependencies (
    owner_kind TEXT NOT NULL CHECK (owner_kind IN ('entry','sentence')),
    owner_id UUID NOT NULL,
    target_id UUID NOT NULL,
    target_path TEXT NOT NULL,
    area TEXT NOT NULL CHECK (area IN ('word','azure','grammar','meaning','example')),
    content_revision BIGINT NOT NULL,
    PRIMARY KEY (owner_kind, owner_id, target_id, target_path, area)
);
CREATE INDEX dubbing_dependencies_area ON speech.dubbing_dependencies(area);
