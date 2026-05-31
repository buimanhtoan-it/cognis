/**
 * In-memory user repository. Backs the auth flow without dragging a real
 * database into the fixture. The SQL-shaped string literals exist so the
 * cognis enricher has something to detect when extracting `db_table`
 * attributes — they are never actually executed.
 */

import { randomUUID } from "node:crypto";

/* ------------------------------------------------------------------------ */
/*  Types                                                                   */
/* ------------------------------------------------------------------------ */

export interface UserRecord {
  id: string;
  username: string;
  email: string;
  passwordHash: string;
  roles: ReadonlyArray<string>;
  createdAt: number;
  updatedAt: number;
  disabled: boolean;
}

export interface NewUserInput {
  username: string;
  email: string;
  passwordHash: string;
  roles?: ReadonlyArray<string>;
}

export interface UserPatch {
  email?: string;
  roles?: ReadonlyArray<string>;
  disabled?: boolean;
}

/* ------------------------------------------------------------------------ */
/*  SQL-shaped query strings (NEVER executed)                               */
/* ------------------------------------------------------------------------ */

/* These exist so the cognis enricher (task 9) finds something to attribute.
 * They mirror what a Postgres-backed repo would issue.
 */

export const SQL_SELECT_USER_BY_ID =
  "SELECT id, username, email, password_hash, roles, created_at, updated_at, disabled FROM users WHERE id = $1";

export const SQL_SELECT_USER_BY_USERNAME =
  "SELECT id, username, email, password_hash, roles, created_at, updated_at, disabled FROM users WHERE username = $1";

export const SQL_INSERT_USER =
  "INSERT INTO users (id, username, email, password_hash, roles, created_at, updated_at, disabled) " +
  "VALUES ($1, $2, $3, $4, $5, $6, $7, $8)";

export const SQL_UPDATE_USER =
  "UPDATE users SET email = COALESCE($2, email), roles = COALESCE($3, roles), " +
  "disabled = COALESCE($4, disabled), updated_at = $5 WHERE id = $1";

export const SQL_DELETE_USER = "DELETE FROM users WHERE id = $1";

export const SQL_LIST_USERS =
  "SELECT id, username, email, roles, disabled FROM users ORDER BY created_at DESC LIMIT $1 OFFSET $2";

/* ------------------------------------------------------------------------ */
/*  Errors                                                                  */
/* ------------------------------------------------------------------------ */

export class UserNotFoundError extends Error {
  constructor(public readonly key: string) {
    super(`user not found: ${key}`);
    this.name = "UserNotFoundError";
  }
}

export class DuplicateUserError extends Error {
  constructor(public readonly username: string) {
    super(`user already exists: ${username}`);
    this.name = "DuplicateUserError";
  }
}

/* ------------------------------------------------------------------------ */
/*  Repository                                                              */
/* ------------------------------------------------------------------------ */

export class UserRepo {
  private readonly byId = new Map<string, UserRecord>();
  private readonly byUsername = new Map<string, string>();

  constructor(seed: ReadonlyArray<UserRecord> = []) {
    for (const record of seed) {
      this.byId.set(record.id, record);
      this.byUsername.set(record.username.toLowerCase(), record.id);
    }
  }

  size(): number {
    return this.byId.size;
  }

  async getById(id: string): Promise<UserRecord> {
    const record = this.byId.get(id);
    if (!record) {
      throw new UserNotFoundError(id);
    }
    // Note: in a real repo this would be SQL_SELECT_USER_BY_ID against pg.
    return { ...record, roles: [...record.roles] };
  }

  async findByUsername(username: string): Promise<UserRecord | undefined> {
    const id = this.byUsername.get(username.toLowerCase());
    if (!id) return undefined;
    const record = this.byId.get(id);
    return record ? { ...record, roles: [...record.roles] } : undefined;
  }

  async create(input: NewUserInput): Promise<UserRecord> {
    const key = input.username.toLowerCase();
    if (this.byUsername.has(key)) {
      throw new DuplicateUserError(input.username);
    }
    const now = Date.now();
    const record: UserRecord = {
      id: randomUUID(),
      username: input.username,
      email: input.email,
      passwordHash: input.passwordHash,
      roles: input.roles ?? ["user"],
      createdAt: now,
      updatedAt: now,
      disabled: false,
    };
    this.byId.set(record.id, record);
    this.byUsername.set(key, record.id);
    return { ...record, roles: [...record.roles] };
  }

  async update(id: string, patch: UserPatch): Promise<UserRecord> {
    const record = this.byId.get(id);
    if (!record) {
      throw new UserNotFoundError(id);
    }
    const merged: UserRecord = {
      ...record,
      email: patch.email ?? record.email,
      roles: patch.roles ?? record.roles,
      disabled: patch.disabled ?? record.disabled,
      updatedAt: Date.now(),
    };
    this.byId.set(id, merged);
    return { ...merged, roles: [...merged.roles] };
  }

  async delete(id: string): Promise<void> {
    const record = this.byId.get(id);
    if (!record) {
      throw new UserNotFoundError(id);
    }
    this.byId.delete(id);
    this.byUsername.delete(record.username.toLowerCase());
  }

  async list(limit: number, offset: number): Promise<ReadonlyArray<UserRecord>> {
    const all = Array.from(this.byId.values()).sort((a, b) => b.createdAt - a.createdAt);
    return all.slice(offset, offset + limit).map((r) => ({ ...r, roles: [...r.roles] }));
  }

  /**
   * Return a deterministic snapshot for tests. Cloned so callers cannot
   * mutate the repo's internal state by holding the result.
   */
  snapshot(): ReadonlyArray<UserRecord> {
    return Array.from(this.byId.values()).map((r) => ({ ...r, roles: [...r.roles] }));
  }
}

/**
 * Build a small seeded repo for fixtures. The hash strings are not real
 * bcrypt output — tests stub the hash comparison.
 */
export function buildSeedRepo(): UserRepo {
  const now = Date.now();
  return new UserRepo([
    {
      id: "00000000-0000-4000-8000-00000000000a",
      username: "alice",
      email: "alice@example.com",
      passwordHash: "$2b$12$placeholderhashforfixturealice0000000000000000000000",
      roles: ["user", "admin"],
      createdAt: now - 86_400_000,
      updatedAt: now - 86_400_000,
      disabled: false,
    },
    {
      id: "00000000-0000-4000-8000-00000000000b",
      username: "bob",
      email: "bob@example.com",
      passwordHash: "$2b$12$placeholderhashforfixturebob000000000000000000000000",
      roles: ["user"],
      createdAt: now - 172_800_000,
      updatedAt: now - 172_800_000,
      disabled: false,
    },
    {
      id: "00000000-0000-4000-8000-00000000000c",
      username: "carol",
      email: "carol@example.com",
      passwordHash: "$2b$12$placeholderhashforfixturecarol000000000000000000000",
      roles: ["user"],
      createdAt: now - 259_200_000,
      updatedAt: now - 259_200_000,
      disabled: true,
    },
  ]);
}
