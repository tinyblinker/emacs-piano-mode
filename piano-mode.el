;;; piano-mode.el --- Play piano with real key-up  -*- lexical-binding: t; -*-

;; Author: shyweeds
;; Maintainer: shyweeds
;; Version: 0.4.1
;; Package-Requires: ((emacs "28.1"))
;; Keywords: games, multimedia
;; URL: https://github.com/tinyblinker/emacs-piano-mode

;;; Commentary:

;; Turn your computer keyboard into a piano with real key-down/key-up
;; tracking.
;;
;; The companion `piano-keys' program (Rust, zero dependencies) reads
;; raw key state from the kernel evdev ABI on Linux without root or
;; `input' group membership, and reports "down KEY" / "up KEY" lines
;; to Emacs.  Emacs maps them to MIDI notes and drives a persistent
;; FluidSynth process.
;;
;; `M-x piano-play' opens the piano buffer and starts everything.

;;; Code:

(require 'seq)
(require 'subr-x)

(defgroup piano nil
  "Play a piano from your computer keyboard."
  :group 'games
  :prefix "piano-")

(defun piano--guess-load-dir ()
  "Return the directory piano-mode was loaded from, or nil.
When `load-file-name' names a native-compiled .eln inside the eln
cache, ask `locate-library' for the installed source directory
instead."
  (let ((file (or load-file-name (buffer-file-name))))
    (cond
     ((and file (not (string-suffix-p ".eln" file)))
      (file-name-directory file))
     ((when-let* ((library (locate-library "piano-mode")))
        (file-name-directory library)))
     (file
      (file-name-directory file)))))

(defvar piano--load-dir (piano--guess-load-dir)
  "Directory the package was loaded from, or nil.
Used to find the SoundFont and the native key reader bundled next
to the loaded file.")

;; Note state and both processes are global: sound comes from a single
;; instrument, no matter which buffer started it.
(defvar piano--notes (make-hash-table :test #'eql)
  "MIDI notes currently sounding.")

(defvar piano--keys-process nil
  "The native key reader process.")

(defvar piano--synth-process nil
  "The FluidSynth process.")

(defvar piano--buffer nil
  "The dedicated buffer created by `piano-play'.")

(defconst piano--keys-stderr " *piano-keys-errors*"
  "Buffer receiving the key reader's standard error.")

(defconst piano--synth-stderr " *piano-fluidsynth-errors*"
  "Buffer receiving FluidSynth's standard error.")

;;; Options

(defun piano--set-helper-option (symbol value)
  "Set SYMBOL to VALUE and restart a live key reader."
  (unless (and (default-boundp symbol)
               (equal value (default-value symbol)))
    (set-default symbol value)
    (when (process-live-p piano--keys-process)
      (piano--restart-helper))))

(defun piano--set-synth-option (symbol value)
  "Set SYMBOL to VALUE and restart a live synthesizer."
  (unless (and (default-boundp symbol)
               (equal value (default-value symbol)))
    (set-default symbol value)
    (when (process-live-p piano--synth-process)
      (piano--restart-synth))))

(defcustom piano-keys
  '(("a" . 60) ("w" . 61) ("s" . 62) ("e" . 63) ("d" . 64)
    ("f" . 65) ("t" . 66) ("g" . 67) ("y" . 68) ("h" . 69)
    ("u" . 70) ("j" . 71) ("k" . 72) ("o" . 73) ("l" . 74)
    ("p" . 75) (";" . 76))
  "Alist mapping key names to MIDI note numbers.

Each element is (KEY . MIDI).  KEY is a single letter, digit or
punctuation character accepted by `piano-keys-program'; letters are
matched case-insensitively.  MIDI is a note number, with middle C as
60.  Changing this restarts a running key reader."
  :group 'piano
  :type '(alist :key-type string :value-type integer)
  :set #'piano--set-helper-option)

(defcustom piano-keys-program "piano-keys"
  "Name (or path) of the native key reader program."
  :group 'piano
  :type 'string
  :set #'piano--set-helper-option)

(defcustom piano-keys-extra-args nil
  "Extra arguments passed to `piano-keys-program'.
For example (\"--device\" \"/dev/input/event3\") pins one keyboard
device."
  :group 'piano
  :type '(repeat string)
  :set #'piano--set-helper-option)

(defcustom piano-auto-build t
  "Whether to build the bundled `piano-keys' reader when it is missing.
The package ships the reader's Rust source under piano_core/.  When
this is non-nil and a Rust toolchain is available, the first call to
`piano-play' builds the reader in place; set it to nil to require a
prebuilt `piano-keys' on the variable `exec-path' or in
`piano-keys-program'."
  :group 'piano
  :type 'boolean)

(defcustom piano-device nil
  "Evdev device path used by the key reader, or nil.

Nil selects the device on the first start: the key reader lists the
keyboards it can see and you pick one with completion; a single
keyboard is picked automatically.  A string pins the reader to that
device, e.g. \"/dev/input/by-id/usb-Keychron_K8_Pro-event-kbd\".
`piano-select-device' re-runs the interactive choice and saves it."
  :group 'piano
  :type '(choice (const :tag "Ask when needed" nil) string)
  :set #'piano--set-helper-option)

(defcustom piano-poll-ms 4
  "Keyboard polling interval in milliseconds."
  :group 'piano
  :type 'integer
  :set #'piano--set-helper-option)

(defcustom piano-transpose 0
  "Semitones added to every note."
  :group 'piano
  :type 'integer)

(defcustom piano-velocity 100
  "MIDI velocity (1-127) sent with every note-on."
  :group 'piano
  :type 'integer)

(defcustom piano-synth-program "fluidsynth"
  "Name (or path) of the FluidSynth executable."
  :group 'piano
  :type 'string
  :set #'piano--set-synth-option)

(defcustom piano-soundfont nil
  "SoundFont file for FluidSynth.
If nil, try `PIANO_SOUNDFONT', a TimGM6mb.sf2 next to the loaded
package, and common system locations."
  :group 'piano
  :type '(choice (const :tag "Auto-detect" nil) file)
  :set #'piano--set-synth-option)

(defcustom piano-audio-driver nil
  "FluidSynth audio driver, e.g. \"alsa\" or \"pipewire\".
Nil lets FluidSynth pick its default."
  :group 'piano
  :type '(choice (const :tag "FluidSynth default" nil) string)
  :set #'piano--set-synth-option)

(defcustom piano-gain 0.5
  "FluidSynth master gain."
  :group 'piano
  :type 'number
  :set #'piano--set-synth-option)

;;; Synthesizer

(defun piano--soundfont ()
  "Return a readable SoundFont file name.
Signal a `user-error' when a configured SoundFont is unreadable or
no SoundFont can be found."
  (let ((configured (or piano-soundfont (getenv "PIANO_SOUNDFONT"))))
    (if configured
        (if (file-readable-p configured)
            configured
          (user-error "SoundFont is not readable: %s" configured))
      (or (seq-find #'file-readable-p
                    (delq nil
                          (list (when piano--load-dir
                                  (expand-file-name
                                   "piano_core/src/TimGM6mb.sf2"
                                   piano--load-dir))
                                "/usr/share/sounds/sf2/FluidR3_GM.sf2"
                                "/usr/share/soundfonts/FluidR3_GM.sf2"
                                "/usr/share/sounds/sf2/default-GM.sf2")))
          (user-error "SoundFont not found; install one or set `piano-soundfont'")))))

(defun piano--synth-send (format-string &rest args)
  "Send the shell command FORMAT-STRING (formatted with ARGS) to FluidSynth."
  (when (process-live-p piano--synth-process)
    (process-send-string piano--synth-process
                         (apply #'format format-string args))))

(defun piano--note-on (midi)
  "Start MIDI note MIDI unless it is already sounding."
  (unless (gethash midi piano--notes)
    (puthash midi t piano--notes)
    (piano--synth-send "noteon 0 %d %d\n" midi piano-velocity)))

(defun piano--note-off (midi)
  "Stop MIDI note MIDI."
  (when (gethash midi piano--notes)
    (remhash midi piano--notes)
    (piano--synth-send "noteoff 0 %d\n" midi)))

(defun piano--all-notes-off ()
  "Release every sounding note."
  (maphash (lambda (midi _active)
             (piano--synth-send "noteoff 0 %d\n" midi))
           piano--notes)
  (clrhash piano--notes))

;;; Keymap

(defvar piano-mode-map
  (let ((map (make-sparse-keymap)))
    ;; The reader watches keys globally; swallow printable input and
    ;; the common editing keys so they cannot change the piano buffer.
    (dotimes (offset 95)
      (define-key map (char-to-string (+ 32 offset)) #'ignore))
    (dolist (key '("\t" "\r" "\d"))
      (define-key map key #'ignore))
    (define-key map [remap self-insert-command] #'ignore)
    map)
  "Keymap for `piano-mode'.")

;;; Key reader

(defun piano--helper-keys-argument ()
  "Return the --keys argument derived from `piano-keys'."
  (mapconcat (lambda (binding) (upcase (car binding))) piano-keys ","))

(defun piano--find-keys-program ()
  "Return the piano-keys executable, or nil.

Checks `piano-keys-program' on the search path, and also the build
outputs inside this repository, so loading the package straight from
a checkout just works."
  (or (executable-find piano-keys-program)
      (when piano--load-dir
        (seq-find #'file-executable-p
                  (list (expand-file-name "piano_core/target/release/piano-keys"
                                          piano--load-dir)
                        (expand-file-name "piano_core/target/debug/piano-keys"
                                          piano--load-dir))))))

(defun piano--keys-source-directory ()
  "Return the bundled piano_core source directory, or nil."
  (when piano--load-dir
    (let ((directory (expand-file-name "piano_core" piano--load-dir)))
      (when (file-exists-p (expand-file-name "Cargo.toml" directory))
        directory))))

(defun piano--find-cargo ()
  "Return the Cargo executable, or nil."
  (or (executable-find "cargo")
      (let ((cargo (expand-file-name "~/.cargo/bin/cargo")))
        (and (file-executable-p cargo) cargo))))

(defun piano--build-keys-program ()
  "Build the bundled piano-keys reader and return its path.
Return nil when auto-building is disabled, `piano-keys-program' is
customized, or no source tree or Rust toolchain is available.
Signal a `user-error' when the build fails."
  (when (and piano-auto-build
             (string= piano-keys-program "piano-keys"))
    (when-let* ((directory (piano--keys-source-directory))
                (cargo (piano--find-cargo)))
      (message "piano-mode: building piano-keys (first run)...")
      (let ((buffer (get-buffer-create " *piano-keys-build*")))
        (with-current-buffer buffer
          (erase-buffer))
        (let ((status (call-process
                       cargo nil buffer nil
                       "build" "--release" "--locked"
                       "--manifest-path"
                       (expand-file-name "Cargo.toml" directory))))
          (unless (and (integerp status) (zerop status))
            (user-error "Building piano-keys failed; see buffer %s"
                        (buffer-name buffer)))))
      (message "piano-mode: piano-keys built")
      (piano--find-keys-program))))

(defun piano--ensure-keys-program ()
  "Return the piano-keys executable, building it when necessary."
  (or (piano--find-keys-program)
      (piano--build-keys-program)
      (user-error "Key reader piano-keys not found (%S); install Rust and \
run \"cargo build --release\" in %s, or set `piano-keys-program'"
                  piano-keys-program
                  (or (piano--keys-source-directory) "piano_core/"))))

(defun piano--parse-devices (text)
  "Return an alist of (LABEL . PATH) for the keyboard lines in TEXT."
  (let ((devices nil))
    (dolist (line (split-string text "\n"))
      (let ((fields (split-string line "\t")))
        (when (equal (nth 2 fields) "yes")
          (let* ((path (nth 0 fields))
                 (name (nth 1 fields))
                 (label (if (string-empty-p (or name ""))
                            path
                          (format "%s (%s)" name path))))
            (push (cons label path) devices)))))
    (nreverse devices)))

(defun piano--list-devices (program)
  "Return an alist of (LABEL . PATH) for the keyboards PROGRAM reports."
  (with-temp-buffer
    ;; stdout and stderr both land here, so a failure's stderr becomes
    ;; part of the error message below.
    (let ((status (call-process program nil t nil "--list-devices")))
      (unless (and (integerp status) (zerop status))
        (user-error
         "Listing keyboards failed (%s); check read access to /dev/input \
(udev uaccess or the input group)"
         (string-trim (buffer-string)))))
    (piano--parse-devices (buffer-string))))

(defun piano--stable-device-path (path)
  "Return a stable by-id/by-path symlink for PATH, or PATH."
  (or (seq-find
       (lambda (link)
         (ignore-errors (file-equal-p link path)))
       (append
        (ignore-errors
          (directory-files "/dev/input/by-id" t "event-kbd\\'"))
        (ignore-errors
          (directory-files "/dev/input/by-path" t "event-kbd\\'"))))
      path))

(defun piano--select-device (program &optional force)
  "Return a keyboard path reported by PROGRAM.
Reuse `piano-device' when PROGRAM still lists it.  Otherwise choose
a keyboard, asking when there are several; a single keyboard is used
without asking.  With FORCE, ignore `piano-device' and choose again."
  (let ((devices (piano--list-devices program))
        (saved (and (not force)
                    (stringp piano-device)
                    (not (string-empty-p piano-device))
                    piano-device)))
    (cond
     ((null devices)
      (user-error
       "No readable keyboard found under /dev/input; add yourself \
to the input group (or grant udev uaccess access) and try again"))
     ((and saved
           (seq-some (lambda (candidate)
                       (ignore-errors
                         (file-equal-p (cdr candidate) saved)))
                     devices))
      (piano--stable-device-path saved))
     (t
      (when saved
        (message "Saved piano keyboard %s is no longer available; choose another"
                 saved)
        (setq piano-device nil))
      (if (null (cdr devices))
          (piano--stable-device-path (cdar devices))
        (let* ((choices (mapcar #'car devices))
               (choice (completing-read "Keyboard device: " choices nil t))
               (path (piano--stable-device-path
                      (cdr (assoc choice devices)))))
          (customize-save-variable 'piano-device path)
          path))))))

(defun piano--device-label ()
  "Return the configured device path, or \"auto-detect\"."
  (if (and (stringp piano-device)
           (not (string-empty-p piano-device)))
      piano-device
    "auto-detect"))

;;;###autoload
(defun piano-select-device ()
  "Choose the keyboard device for `piano-mode' and save the choice."
  (interactive)
  (let* ((program (piano--ensure-keys-program))
         (device (piano--select-device program t)))
    ;; A prompt already saved the choice and restarted a live reader.
    ;; Saving here covers a single keyboard; the setter skips an
    ;; unchanged value.
    (customize-save-variable 'piano-device device)
    ;; If the reader had died, start it again now that the mode is
    ;; still active.
    (when (and (piano--mode-running-p)
               (not (process-live-p piano--keys-process)))
      (piano--restart-helper))
    (message "Piano keyboard device: %s" device)))

;;; Key events

(defun piano--buffer-selected-p ()
  "Return non-nil when the dedicated piano buffer is selected."
  (and (buffer-live-p piano--buffer)
       (eq (window-buffer (selected-window)) piano--buffer)))

(defun piano--window-state-changed (_window)
  "Release all notes when the piano buffer stops being selected."
  (unless (eq (window-buffer (selected-window)) piano--buffer)
    (piano--all-notes-off)))

(defun piano--apply-key-event (action key)
  "Turn an ACTION (\"down\"/\"up\") event for KEY into note-on/note-off.
KEY names are matched case-insensitively, because the native reader
always reports letters in upper case.  Key-up events are always
processed so a note cannot stick when the buffer loses focus."
  (let ((midi (cdr (assoc-string key piano-keys t))))
    (when midi
      (let ((note (+ midi piano-transpose)))
        (pcase action
          ("down" (when (piano--buffer-selected-p)
                    (piano--note-on note)))
          ("up" (piano--note-off note)))))))

(defun piano--handle-keys-line (line)
  "Dispatch one LINE of key-reader output."
  (pcase (split-string line " " t)
    (`(,action ,key) (piano--apply-key-event action key))
    (_ (unless (string-empty-p line)
         (message "Piano-keys: unrecognized line %S" line)))))

(defun piano--keys-filter (process string)
  "Parse \"down KEY\" / \"up KEY\" lines from the key reader PROCESS.
STRING is the latest chunk of process output; incomplete lines are
kept in a process property until they are finished."
  (let ((pending (concat (process-get process 'piano-pending) string)))
    (while (string-match "\n" pending)
      ;; Grab both substrings before dispatching: the handler calls
      ;; regex functions that would clobber the match data.
      (let ((line (substring pending 0 (match-beginning 0))))
        (setq pending (substring pending (match-end 0)))
        (piano--handle-keys-line line)))
    (process-put process 'piano-pending pending)))

;;; Processes

(defun piano--stderr (name)
  "Return non-empty trimmed standard error from buffer NAME, or nil."
  (when-let* ((buffer (get-buffer name)))
    (let ((text (with-current-buffer buffer
                  (string-trim (buffer-string)))))
      (unless (string-empty-p text) text))))

(defun piano--reset-stderr (name)
  "Clear the standard error buffer NAME."
  (with-current-buffer (get-buffer-create name)
    (erase-buffer)))

(defun piano--keys-sentinel (process event)
  "Handle the key reader PROCESS exiting with EVENT."
  (when (memq (process-status process) '(exit signal))
    (when (eq process piano--keys-process)
      (setq piano--keys-process nil))
    (piano--all-notes-off)
    (message "The piano-keys reader exited: %s; run M-x piano-select-device"
             (or (piano--stderr piano--keys-stderr)
                 (string-trim event)))))

(defun piano--start-helper ()
  "Start the native key reader."
  (unless (process-live-p piano--keys-process)
    (let* ((program (piano--ensure-keys-program))
           (device (piano--select-device program)))
      (piano--reset-stderr piano--keys-stderr)
      (let ((process
             (make-process
              :name "piano-keys"
              :command (append (list program
                                     "--keys" (piano--helper-keys-argument)
                                     "--poll-ms" (number-to-string piano-poll-ms))
                               (when device (list "--device" device))
                               piano-keys-extra-args)
              :connection-type 'pipe
              :coding 'utf-8-unix
              :noquery t
              :buffer (get-buffer-create " *piano-keys*")
              :stderr (get-buffer-create piano--keys-stderr)
              :filter #'piano--keys-filter
              :sentinel #'ignore)))
        (setq piano--keys-process process)
        ;; Catch a reader that exits immediately: bad device, permission
        ;; problem, or an unsupported key name.
        (accept-process-output process 0.1)
        (if (process-live-p process)
            (set-process-sentinel process #'piano--keys-sentinel)
          (setq piano--keys-process nil)
          (user-error "Piano-keys exited during startup: %s"
                      (or (piano--stderr piano--keys-stderr)
                          "unknown error")))))))

(defun piano--mode-running-p ()
  "Return non-nil when the piano buffer is still in `piano-mode'."
  (and piano--buffer
       (buffer-live-p piano--buffer)
       (buffer-local-value 'piano-mode piano--buffer)))

(defun piano--restart-helper ()
  "Restart the key reader when the mode is active."
  (when (or (process-live-p piano--keys-process)
            (piano--mode-running-p))
    (piano--all-notes-off)              ; keys held on the old device
    (when (process-live-p piano--keys-process)
      (set-process-sentinel piano--keys-process #'ignore)
      (delete-process piano--keys-process))
    (setq piano--keys-process nil)
    (piano--start-helper)))

(defun piano--synth-sentinel (process event)
  "Handle FluidSynth PROCESS exiting with EVENT."
  (when (memq (process-status process) '(exit signal))
    (when (eq process piano--synth-process)
      (setq piano--synth-process nil))
    ;; The notes died with the synthesizer.
    (clrhash piano--notes)
    (message "FluidSynth exited: %s"
             (or (piano--stderr piano--synth-stderr)
                 (string-trim event)))))

(defun piano--start-synth ()
  "Start FluidSynth unless it is already running.

Signal a `user-error' when FluidSynth exits during startup, e.g. on
a bad SoundFont or an unusable audio driver."
  (unless (process-live-p piano--synth-process)
    (let ((program (or (executable-find piano-synth-program)
                       (user-error "FluidSynth not found (%S); install it first"
                                   piano-synth-program)))
          (soundfont (piano--soundfont)))
      (piano--reset-stderr piano--synth-stderr)
      (let ((process
             (make-process
              :name "piano-fluidsynth"
              :command (append (list program
                                     "-n" "-g" (number-to-string piano-gain))
                               (when piano-audio-driver
                                 (list "-a" piano-audio-driver))
                               (list soundfont))
              :connection-type 'pipe
              :coding 'utf-8-unix
              :noquery t
              :buffer (get-buffer-create " *piano-fluidsynth*")
              :stderr (get-buffer-create piano--synth-stderr)
              :sentinel #'ignore)))
        (setq piano--synth-process process)
        ;; Give it a moment to start: FluidSynth exits immediately on
        ;; a bad SoundFont or unusable audio driver.
        (accept-process-output process 0.3)
        (if (process-live-p process)
            (set-process-sentinel process #'piano--synth-sentinel)
          (setq piano--synth-process nil)
          (user-error "FluidSynth exited during startup: %s"
                      (or (piano--stderr piano--synth-stderr)
                          "unknown error")))))))

(defun piano--stop-synth ()
  "Stop FluidSynth, releasing all notes first."
  (when (process-live-p piano--synth-process)
    (let ((process piano--synth-process))
      (set-process-sentinel process #'ignore)
      (ignore-errors
        (piano--synth-send "cc 0 123 0\nquit\n"))
      (accept-process-output process 0.2)
      (when (process-live-p process)
        (delete-process process))))
  (setq piano--synth-process nil))

(defun piano--restart-synth ()
  "Restart FluidSynth when the mode is active."
  (when (or (process-live-p piano--synth-process)
            (piano--mode-running-p))
    (piano--stop-synth)
    (piano--start-synth)))

(defun piano--start ()
  "Start the synthesizer and the key reader."
  (piano--start-synth)
  (piano--start-helper))

(defun piano--stop ()
  "Stop the key reader, release all notes, and stop the synthesizer."
  (when (process-live-p piano--keys-process)
    (set-process-sentinel piano--keys-process #'ignore)
    (delete-process piano--keys-process))
  (setq piano--keys-process nil
        piano--buffer nil)
  (piano--all-notes-off)
  (piano--stop-synth))

(defun piano--kill-buffer ()
  "Stop piano mode when its buffer is killed."
  (when (and (boundp 'piano-mode) piano-mode)
    (piano-mode -1)))

;;;###autoload
(define-minor-mode piano-mode
  "Play piano notes from the computer keyboard.
\\{piano-mode-map}"
  :lighter " Piano"
  :keymap piano-mode-map
  (if piano-mode
      (condition-case error
          (piano--start)
        (error
         ;; A failed start must not leak the synthesizer process.
         (setq piano-mode nil)
         (piano--stop)
         (signal (car error) (cdr error))))
    (piano--stop)))

;;;###autoload
(defun piano-play ()
  "Open a dedicated piano buffer and start playing."
  (interactive)
  (let ((buffer (get-buffer-create "*piano*")))
    (pop-to-buffer buffer)
    (setq piano--buffer buffer)
    (with-current-buffer buffer
      (unless (derived-mode-p 'special-mode)
        (special-mode))
      (add-hook 'kill-buffer-hook #'piano--kill-buffer nil t)
      ;; Release held notes when the buffer stops being displayed or
      ;; selected; key-up events also release them on their own.
      (add-hook 'window-buffer-change-functions
                #'piano--window-state-changed nil t)
      (add-hook 'window-selection-change-functions
                #'piano--window-state-changed nil t)
      (piano-mode 1)
      (piano--show-keys))))

(defun piano--show-keys ()
  "Insert the current key layout into the current buffer."
  (let ((inhibit-read-only t))
    (erase-buffer)
    (insert "piano-mode — the keys below are read globally by piano-keys\n"
            (format "keys: %s\n"
                    (mapconcat (lambda (binding) (upcase (car binding)))
                               piano-keys " "))
            (format "device: %s\n" (piano--device-label))
            "M-x piano-mode stops the instrument; kill this buffer to quit\n")))

;;;###autoload
(defun piano-test-note (&optional midi)
  "Play MIDI note MIDI (default 60) briefly, to test the sound path."
  (interactive)
  (let ((note (or midi 60)))
    (unless (process-live-p piano--synth-process)
      (piano--start-synth))
    (piano--note-on note)
    (run-at-time 0.8 nil #'piano--note-off note)))

(add-hook 'kill-emacs-hook #'piano--stop)

(provide 'piano-mode)
;;; piano-mode.el ends here
