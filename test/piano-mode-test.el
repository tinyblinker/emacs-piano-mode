;;; piano-mode-test.el --- Tests for piano-mode  -*- lexical-binding: t; -*-

;; Run with:
;;   emacs --batch -Q -L . -l test/piano-mode-test.el \
;;         -f ert-run-tests-batch-and-exit

(require 'ert)
(require 'cl-lib)
(require 'piano-mode)

(defconst piano-test-dir
  (file-name-directory (or load-file-name buffer-file-name)))

(defconst piano-test-bin
  (expand-file-name "fixtures/bin" piano-test-dir))

(defconst piano-test-soundfont
  (expand-file-name "fixtures/dummy-soundfont" piano-test-dir))

(defun piano-test-wait-for (predicate &optional seconds)
  "Wait up to SECONDS (default 5) for PREDICATE to become non-nil.
Return PREDICATE's final value."
  (let ((deadline (+ (float-time) (or seconds 5))))
    (while (and (not (funcall predicate)) (< (float-time) deadline))
      (sit-for 0.05))
    (funcall predicate)))

(defun piano-test-slurp (file)
  "Return the contents of FILE."
  (with-temp-buffer
    (insert-file-contents file)
    (buffer-string)))

(defmacro piano-test-environment (&rest body)
  "Run BODY with fake helper/synthesizer programs and clean state."
  `(let ((piano-test-log (make-temp-file "piano-synth" nil ".log")))
     (unwind-protect
         (let ((exec-path (cons piano-test-bin exec-path))
               (piano-keys-program "piano-keys")
               (piano-synth-program "fluidsynth")
               (piano-soundfont piano-test-soundfont)
               (piano-poll-ms 4)
               (process-environment
                (cons (format "PIANO_SYNTH_LOG=%s" piano-test-log)
                      process-environment)))
           (clrhash piano--active)
           ,@body)
       (delete-file piano-test-log))))

(ert-deftest piano-test-helper-pipeline ()
  "A down/up pair from piano-keys becomes noteon/noteoff."
  (piano-test-environment
    (with-temp-buffer
      (set-window-buffer (selected-window) (current-buffer))
      (let ((piano--piano-buffer (current-buffer)))
        (piano-mode 1)
        (unwind-protect
            (progn
              (should (piano-test-wait-for
                       (lambda ()
                         (let ((log (piano-test-slurp piano-test-log)))
                           (and (string-match-p "noteon 0 60 100" log)
                                (string-match-p "noteoff 0 60" log))))))
              (should (= 0 (hash-table-count piano--active))))
          (piano-mode -1))))))

(ert-deftest piano-test-stop-kills-processes ()
  "Disabling the mode stops both helper processes."
  (piano-test-environment
    (with-temp-buffer
      (piano-mode 1)
      (unwind-protect
          (progn
            (should (process-live-p piano--keys-process))
            (should (process-live-p piano--synth-process)))
        (piano-mode -1))
      (should-not (process-live-p piano--keys-process))
      (should-not (process-live-p piano--synth-process)))))

(ert-deftest piano-test-lines-are-dispatched ()
  "down/up lines map to noteon/noteoff; unknown keys are ignored."
  (let ((calls '()))
    (clrhash piano--active)
    (cl-letf (((symbol-function 'piano--synth-send)
               (lambda (format-string &rest args)
                 (push (apply #'format format-string args) calls)))
              ((symbol-function 'piano--helper-active-p)
               (lambda () t)))
      (piano--handle-keys-line "down A")
      (piano--handle-keys-line "up A")
      (piano--handle-keys-line "down Q")
      (piano--handle-keys-line "garbage"))
    (should (member "noteon 0 60 100\n" calls))
    (should (member "noteoff 0 60\n" calls))
    (should (= 0 (hash-table-count piano--active)))))

(ert-deftest piano-test-transpose ()
  "`piano-transpose' shifts the MIDI note."
  (let ((calls '())
        (piano-transpose 2))
    (clrhash piano--active)
    (cl-letf (((symbol-function 'piano--synth-send)
               (lambda (format-string &rest args)
                 (push (apply #'format format-string args) calls)))
              ((symbol-function 'piano--helper-active-p)
               (lambda () t)))
      (piano--handle-keys-line "down A"))
    (should (member "noteon 0 62 100\n" calls))
    (should (member 62 (hash-table-keys piano--active)))))

(ert-deftest piano-test-keys-argument ()
  "The --keys argument carries every mapped key, upcased."
  (should (equal (piano--helper-keys-argument)
                 "A,W,S,E,D,F,T,G,Y,H,U,J,K,O,L,P,;")))

(ert-deftest piano-test-find-keys-program-repo ()
  "A sibling build inside the repository is found without PATH."
  (let ((dir (make-temp-file "piano-repo" t)))
    (unwind-protect
        (let* ((binary (expand-file-name "piano_core/target/debug/piano-keys" dir))
               (piano--load-dir dir))
          (make-directory (expand-file-name "piano_core/target/debug" dir) t)
          (write-region "" nil binary)
          (set-file-modes binary #o755)
          (cl-letf (((symbol-function 'executable-find)
                     (lambda (_name) nil)))
            (should (equal (piano--find-keys-program) binary))))
      (delete-directory dir t))))

(ert-deftest piano-test-parse-devices ()
  "Keyboard lines in --list-devices output become (LABEL . PATH) pairs."
  (should
   (equal
    (piano--parse-devices
     (concat "/dev/input/event1\tFake Keyboard\tyes\n"
             "/dev/input/event2\tFake Mouse\tno\n"
             "/dev/input/event3\t\tno\n"))
    '(("Fake Keyboard (/dev/input/event1)" . "/dev/input/event1")))))

(ert-deftest piano-test-select-device-single ()
  "A single candidate device is picked without prompting."
  (let ((piano-device nil)
        (piano--device nil)
        (piano--device-chosen nil))
    (cl-letf (((symbol-function 'piano--list-devices)
               (lambda (_program)
                 '(("Only (/dev/input/event3)" . "/dev/input/event3")))))
      (should (equal (piano--select-device "piano-keys")
                     "/dev/input/event3")))))

(ert-deftest piano-test-select-device-prompt ()
  "With several candidates the completion choice wins."
  (let ((piano-device nil)
        (piano--device nil)
        (piano--device-chosen nil))
    (cl-letf (((symbol-function 'piano--list-devices)
               (lambda (_program)
                 '(("Keyboard B (/dev/input/event5)" . "/dev/input/event5")
                   ("Keyboard A (/dev/input/event3)" . "/dev/input/event3"))))
              ((symbol-function 'completing-read)
               (lambda (_prompt choices &rest _args)
                 (should (member "Auto-detect (first match)" choices))
                 "Keyboard A (/dev/input/event3)")))
      (should (equal (piano--select-device "piano-keys")
                     "/dev/input/event3")))))

(ert-deftest piano-test-helper-active-gate ()
  "Key events are gated on the piano buffer existing and being focused."
  (with-temp-buffer
    (set-window-buffer (selected-window) (current-buffer))
    (let ((piano--piano-buffer nil))
      (should-not (piano--helper-active-p))       ; no piano buffer yet
      (setq piano--piano-buffer (current-buffer))
      (should (piano--helper-active-p))           ; piano buffer focused
      (set-window-buffer (selected-window) (get-buffer-create "*scratch*"))
      (should-not (piano--helper-active-p)))))    ; focus moved away

(ert-deftest piano-test-select-device-auto ()
  "Choosing auto-detect yields nil, so no --device is passed."
  (let ((piano-device nil)
        (piano--device nil)
        (piano--device-chosen nil))
    (cl-letf (((symbol-function 'piano--list-devices)
               (lambda (_program)
                 '(("A (/dev/input/event3)" . "/dev/input/event3")
                   ("B (/dev/input/event5)" . "/dev/input/event5"))))
              ((symbol-function 'completing-read)
               (lambda (_prompt _choices &rest _args)
                 "Auto-detect (first match)")))
      (should (equal (piano--select-device "piano-keys") nil))
      (should piano--device-chosen))))

;;; piano-mode-test.el ends here
