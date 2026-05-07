#!/usr/bin/env node

// @ts-check

/**
 * This is a wrapper script for running the resolc Wasm build.
 *
 * It reads the standard JSON input from stdin, compiles the contracts
 * to PolkaVM bytecode, and writes the standard JSON output to stdout.
 */

"use strict";

const fs = require("node:fs");
const path = require("node:path");
const process = require("node:process");
const util = require("node:util");

const USAGE = "Usage: node run-resolc-wasm.cjs --resolc <path> --solc <path> < input.json";

/**
 * The resolc Wasm compiler.
 *
 * @typedef {Object} ResolcWasm
 * @property {unknown} soljson - The loaded soljson Emscripten module.
 * @property {(data: string) => void} writeToStdin - Writes data to stdin.
 * @property {(args: string[]) => number} callMain - Invokes the compiler and returns the exit code.
 * @property {() => string} readFromStdout - Reads the stdout content.
 * @property {() => string} readFromStderr - Reads the stderr content.
 */

/**
* A custom error for invalid usage.
*/
class ValidationError extends Error {
    /**
     * @param {string} message - The error message.
     * @param {{ showUsage: boolean }} [options] - Whether to show usage information.
     */
    constructor(message, { showUsage } = { showUsage: false }) {
        message += showUsage ? `\n\n${USAGE}` : "";
        super(message);
        this.name = "ValidationError";
    }
}

/**
 * The parsed CLI arguments.
 *
 * @typedef {Object} ParsedArguments
 * @property {string} resolcPath - Resolved path to the resolc Node.js module.
 * @property {string} soljsonPath - Resolved path to the soljson Node.js module.
 */

/**
 * Parses, resolves, and validates the command-line arguments.
 *
 * @returns {ParsedArguments} The parsed arguments.
 * @throws {ValidationError} If any argument is invalid.
 */
function parseArguments() {
    try {
        const { resolc, solc } = util.parseArgs({
            options: {
                resolc: { type: "string" },
                solc: { type: "string" },
            },
            strict: true,
        }).values;

        if (!resolc || !solc) {
            throw new Error("Missing required argument");
        }

        const resolcPath = path.resolve(resolc);
        if (!fs.existsSync(resolcPath)) {
            throw new Error(`resolc not found: ${resolcPath}`);
        }
        const soljsonPath = path.resolve(solc);
        if (!fs.existsSync(soljsonPath)) {
            throw new Error(`solc not found: ${soljsonPath}`);
        }

        return { resolcPath, soljsonPath };
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        throw new ValidationError(message, { showUsage: true });
    }
}

/**
 * Loads the Node.js modules from the provided paths.
 *
 * @param {string} resolcPath - The path to the resolc Node.js module.
 * @param {string} soljsonPath - The path to the soljson Node.js module.
 * @returns {{ createResolc: () => ResolcWasm, soljson: unknown }} A resolc Wasm compiler factory and the loaded soljson module.
 * @throws {ValidationError} If the modules are invalid.
 */
function loadModules(resolcPath, soljsonPath) {
    try {
        const createResolc = require(resolcPath);
        const soljson = require(soljsonPath);
        if (typeof createResolc !== "function") {
            throw new Error(`The resolc module '${resolcPath}' did not export a function`);
        }
        return { createResolc, soljson };
    } catch (error) {
        const message = error instanceof Error ? error.message : String(error);
        throw new ValidationError(`Failed to load module: ${message}`);
    }
}

/**
 * Compiles the source files in the standard JSON input via resolc Wasm.
 *
 * @param {() => ResolcWasm} createResolc - The resolc Wasm compiler factory.
 * @param {unknown} soljson - The loaded soljson Emscripten module.
 * @param {string} input - The standard JSON input.
 * @returns {string} The standard JSON output.
 */
function compile(createResolc, soljson, input) {
    const compiler = createResolc();
    compiler.soljson = soljson;
    compiler.writeToStdin(input);

    const exitCode = compiler.callMain(["--standard-json"]);
    if (exitCode !== 0) {
        throw new Error(`Compilation exited with code ${exitCode}: ${compiler.readFromStderr()}`);
    }

    return compiler.readFromStdout();
}

/**
 * The main entry point.
 * Initiates argument parsing, validation, compilation, and writes the raw result to stdout.
 */
function main() {
    const { resolcPath, soljsonPath } = parseArguments();
    const { createResolc, soljson } = loadModules(resolcPath, soljsonPath);
    const input = fs.readFileSync(0, "utf-8");
    const output = compile(createResolc, soljson, input);
    process.stdout.write(output);
}

try {
    main();
} catch (error) {
    // Include the full stack trace for unexpected (non-ValidationError) exceptions.
    console.error(error instanceof ValidationError ? String(error) : error);
    process.exit(1);
}
