/**
 * Copyright (c) 2017, Daniel Imms (MIT License).
 * Copyright (c) 2018, Microsoft Corporation (MIT License).
 */

import * as fs from 'fs';
import * as assert from 'assert';
import { WindowsTerminal } from './windowsTerminal';
import * as path from 'path';
import * as psList from 'ps-list';
import { expect } from 'chai';

interface IProcessState {
  // Whether the PID must exist or must not exist
  [pid: number]: boolean;
}

interface IWindowsProcessTreeResult {
  name: string;
  pid: number;
}

function pollForProcessState(desiredState: IProcessState, intervalMs: number = 100, timeoutMs: number = 5000): void {
  let tries = 0;
  const interval = setInterval(() => {
    psList({ all: true }).then(ps => {
      let success = true;
      const pids = Object.keys(desiredState).map(k => parseInt(k, 10));
      pids.forEach(pid => {
        if (desiredState[pid]) {
          if (!ps.some(p => p.pid === pid)) {
            success = false;
          }
        } else {
          if (ps.some(p => p.pid === pid)) {
            success = false;
          }
        }
      });
      if (success) {
        clearInterval(interval);
        return;
      }
      tries++;
      if (tries * intervalMs >= timeoutMs) {
        clearInterval(interval);
        const processListing = pids.map(k => `${k}: ${desiredState[k]}`).join('\n');
        assert.fail(`Bad process state, expected:\n${processListing}`);
      }
    });
  }, intervalMs);
}

function pollForProcessTreeSize(pid: number, size: number, intervalMs: number = 100, timeoutMs: number = 2000): Promise<IWindowsProcessTreeResult[]> {
  return new Promise<IWindowsProcessTreeResult[]>(resolve => {
    let tries = 0;
    const interval = setInterval(async () => {
      const ps = await psList({ all: true });
      const openList: IWindowsProcessTreeResult[] = [];
      const filtered = ps
        .filter(p => p.pid === pid)
        .map(p => ({ name: p.name, pid: p.pid }));
      if (filtered.length > 0) {
        openList.push(filtered[0]);
      }
      const list: IWindowsProcessTreeResult[] = [];
      while (openList.length) {
        const current = openList.shift();
        ps.filter(p => p.ppid === current.pid)
          .map(p => ({ name: p.name, pid: p.pid }))
          .forEach(p => openList.push(p));
        list.push(current);
      }
      const success = list.length === size;
      if (success) {
        clearInterval(interval);
        resolve(list);
        return;
      }
      tries++;
      if (tries * intervalMs >= timeoutMs) {
        clearInterval(interval);
        assert.fail(`Bad process state, expected: ${size}, actual: ${list.length}`);
      }
    }, intervalMs);
  });
}

if (process.platform === 'win32') {
  describe('WindowsTerminal', () => {
    describe('kill', () => {
      it('should not crash parent process', function (done) {
        const term = new WindowsTerminal('cmd.exe', [], {});
        term.kill();
        // Add done call to deferred function queue to ensure the kill call has completed
        (<any>term)._defer(done);
      });
      it('should kill the process tree', function (done: Mocha.Done) {
        this.timeout(5000);
        const term = new WindowsTerminal('cmd.exe', [], {});
        // Start sub-processes
        term.write('powershell.exe\r');
        term.write('notepad.exe\r');
        term.write('node.exe\r');
        pollForProcessTreeSize(term.pid, 4, 500, 5000).then(async list=>{
          assert.strictEqual(list[0].name, 'cmd.exe');
          assert.strictEqual(list[1].name, 'powershell.exe');
          assert.strictEqual(list[2].name, 'notepad.exe');
          assert.strictEqual(list[3].name, 'node.exe');
          term.kill();
          const desiredState: IProcessState = {};
          desiredState[list[0].pid] = false;
          desiredState[list[1].pid] = false;
          desiredState[list[2].pid] = true;
          desiredState[list[3].pid] = false;
          pollForProcessState(desiredState);
          // Kill notepad before done
          process.kill(list[2].pid);
          (<any>term)._defer(done);
        });
      });
    });

    describe('resize', () => {
      it('should throw a non-native exception when resizing an invalid value', () => {
        const term = new WindowsTerminal('cmd.exe', [], {});
        assert.throws(() => term.resize(-1, -1));
        assert.throws(() => term.resize(0, 0));
        assert.doesNotThrow(() => term.resize(1, 1));
      });
      it('should throw an non-native exception when resizing a killed terminal', (done) => {
        const term = new WindowsTerminal('cmd.exe', [], {});
        (<any>term)._defer(() => {
          term.on('exit', () => {
            assert.throws(() => term.resize(1, 1));
            done();
          });
          term.destroy();
        });
      });
    });

    describe('Args as CommandLine', () => {
      it('should not fail running a file containing a space in the path', (done) => {
        const spaceFolder = path.resolve(__dirname, '..', 'fixtures', 'space folder');
        if (!fs.existsSync(spaceFolder)) {
          fs.mkdirSync(spaceFolder);
        }

        const cmdCopiedPath = path.resolve(spaceFolder, 'cmd.exe');
        const data = fs.readFileSync(`${process.env.windir}\\System32\\cmd.exe`);
        fs.writeFileSync(cmdCopiedPath, data);

        if (!fs.existsSync(cmdCopiedPath)) {
          // Skip test if git bash isn't installed
          return;
        }
        const term = new WindowsTerminal(cmdCopiedPath, '/C echo "hello world"', {});
        let result = '';
        term.on('data', (data) => {
          result += data;
        });
        term.on('exit', () => {
          expect(result).to.contain('hello world');
          done();
        });
      });
    });

    describe('env', () => {
      it('should place text into a file', (done) => {
        const userProfile = process.env.USERPROFILE;
        const newEnv = { ...process.env, FOO: 'BAR' };
        const term = new WindowsTerminal('cmd.exe', '/C echo %FOO% > %USERPROFILE%\\out.txt',
          { env: newEnv } );
        term.on('exit', () => {
          let result = '';
          try {
            result = fs.readFileSync(`${userProfile}\\out.txt`, 'utf8');
            fs.unlinkSync(`${userProfile}\\out.txt`);
          } catch (err) {
            result = err.toString();
          }
          expect(result).to.contain('BAR');
          done();
        });
      });
      it('should set environment variables of the shell', (done) => {
        const newEnv = { ...process.env, FOO: 'BAR' };
        const term = new WindowsTerminal('cmd.exe', '/C echo %FOO%', { env: newEnv } );
        let result = '';
        term.on('data', (data) => result += data);
        term.on('exit', () => {
          expect(result).to.contain('BAR');
          done();
        });
      });
    });

    describe('On close', () => {
      it('should return process zero exit codes', (done) => {
        const term = new WindowsTerminal('cmd.exe', '/C exit');
        term.on('exit', (code) => {
          assert.strictEqual(code, 0);
          done();
        });
      });

      it('should return process non-zero exit codes', (done) => {
        const term = new WindowsTerminal('cmd.exe', '/C exit 2');
        term.on('exit', (code) => {
          assert.strictEqual(code, 2);
          done();
        });
      });
    });

    describe('winpty', () => {
      it('should accept input', (done) => {
        const term = new WindowsTerminal('cmd.exe', '', { useConpty: false });
        term.write('exit\r');
        term.on('exit', () => {
          done();
        });
      });
    });
  });
}
