#!/usr/bin/env bash
set -eo pipefail

# Directory where backups will be stored.
# Mount this directory in a safe location or upload its contents to cloud storage.
BACKUP_DIR="./data/backups"
DATA_DIR="./data"

mkdir -p "$BACKUP_DIR"

DATE=$(date +%Y-%m-%d_%H-%M-%S)
echo "Starting backup at $DATE"

# 1. Backup SQLite
# Using SQLite's online backup API via the sqlite3 CLI ensures a consistent snapshot
# without stopping the bot. If sqlite3 is not available, we fall back to a simple copy
# (though simple copy with WAL mode can sometimes be inconsistent if active).
SQLITE_DB="$DATA_DIR/bot.db"
SQLITE_BACKUP="$BACKUP_DIR/bot_$DATE.db"

if [ -f "$SQLITE_DB" ]; then
    if command -v sqlite3 >/dev/null 2>&1; then
        echo "Backing up SQLite database using sqlite3 vacuum into..."
        sqlite3 "$SQLITE_DB" "VACUUM INTO '$SQLITE_BACKUP';"
    else
        echo "sqlite3 CLI not found, falling back to direct copy..."
        cp "$SQLITE_DB" "$SQLITE_BACKUP"
    fi
    echo "SQLite backup created: $SQLITE_BACKUP"
else
    echo "SQLite database not found at $SQLITE_DB, skipping."
fi

# 2. Backup Qdrant
# Qdrant supports creating snapshots via its REST API.
QDRANT_HOST=${QDRANT_HOST:-"localhost:6333"}
echo "Creating Qdrant snapshots via API at $QDRANT_HOST..."

# List all collections
COLLECTIONS=$(curl -s "http://$QDRANT_HOST/collections" | grep -oP '"name":"\([^"]*\)"' | cut -d'"' -f4 || true)

if [ -z "$COLLECTIONS" ]; then
    echo "No Qdrant collections found or Qdrant is unreachable."
else
    for collection in $COLLECTIONS; do
        echo "Snapshotting collection: $collection"
        curl -s -X POST "http://$QDRANT_HOST/collections/$collection/snapshots" > /dev/null
    done
    
    # After snapshots are created, they are stored in the Qdrant storage directory
    # which should be mounted at ./data/qdrant_storage/snapshots
    QDRANT_SNAPSHOTS_DIR="$DATA_DIR/qdrant_storage/snapshots"
    if [ -d "$QDRANT_SNAPSHOTS_DIR" ]; then
        QDRANT_BACKUP_TAR="$BACKUP_DIR/qdrant_snapshots_$DATE.tar.gz"
        echo "Archiving Qdrant snapshots to $QDRANT_BACKUP_TAR..."
        tar -czf "$QDRANT_BACKUP_TAR" -C "$DATA_DIR/qdrant_storage" snapshots
        echo "Qdrant backup created: $QDRANT_BACKUP_TAR"
    else
        echo "Qdrant snapshots directory not found at $QDRANT_SNAPSHOTS_DIR."
    fi
fi

# 3. Cleanup old backups (keep last 7 days)
echo "Cleaning up backups older than 7 days..."
find "$BACKUP_DIR" -type f -name "bot_*.db" -mtime +7 -exec rm {} \;
find "$BACKUP_DIR" -type f -name "qdrant_snapshots_*.tar.gz" -mtime +7 -exec rm {} \;

echo "Backup completed successfully."
