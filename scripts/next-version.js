#!/usr/bin/env node
const { execFileSync } = require("child_process");

function readArg(name, fallback = null) {
  const index = process.argv.indexOf(name);
  if (index !== -1 && process.argv[index + 1] !== undefined) {
    return process.argv[index + 1];
  }
  return fallback;
}

function latestTag() {
  try {
    return execFileSync(
      "git",
      ["describe", "--tags", "--abbrev=0", "--match", "v*"],
      {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "ignore"],
      }
    ).trim();
  } catch {
    return "";
  }
}

function parse(version) {
  const match = /^(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$/.exec(version);
  if (!match) {
    throw new Error(`Invalid version: "${version}"`);
  }
  return {
    major: Number(match[1]),
    minor: Number(match[2]),
    patch: Number(match[3]),
    prerelease: match[4] ? match[4].split(".") : [],
  };
}

function bump(version, type, preid) {
  const v = parse(version);
  const base = `${v.major}.${v.minor}.${v.patch}`;
  const pre = (nextBase) => `${nextBase}-${preid}.0`;

  switch (type) {
    case "patch":
      return `${v.major}.${v.minor}.${v.patch + 1}`;
    case "minor":
      return `${v.major}.${v.minor + 1}.0`;
    case "major":
      return `${v.major + 1}.0.0`;
    case "prepatch":
      return pre(`${v.major}.${v.minor}.${v.patch + 1}`);
    case "preminor":
      return pre(`${v.major}.${v.minor + 1}.0`);
    case "premajor":
      return pre(`${v.major + 1}.0.0`);
    case "prerelease": {
      if (v.prerelease.length === 0) {
        return pre(`${v.major}.${v.minor}.${v.patch + 1}`);
      }
      if (preid && v.prerelease[0] !== preid) {
        return pre(base);
      }
      const parts = [...v.prerelease];
      const last = parts[parts.length - 1];
      if (/^\d+$/.test(last)) {
        parts[parts.length - 1] = String(Number(last) + 1);
        return `${base}-${parts.join(".")}`;
      }
      return `${base}-${parts.join(".")}.0`;
    }
    default:
      throw new Error(`Unknown bump type: "${type}"`);
  }
}

const current = readArg("--current", latestTag().replace(/^v/, ""));
const type = readArg("--bump", "patch");
const preid = readArg("--preid", "alpha");

try {
  process.stdout.write(`${bump(current || "0.0.0", type, preid)}\n`);
} catch (error) {
  console.error(error.message);
  process.exit(1);
}
