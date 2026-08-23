//! Full implementation of the diagnostic code system (D-DIAG-01/D-DIAG-02). Reason for using
//! an enum rather than a number: diagnostic codes form an existing closed set that SPEC
//! requires to be "stable and machine-readable", and using Rust's exhaustiveness checking
//! (which warns on non-exhaustive `match` arms) lets the premise "this phase emits only these
//! codes" be guaranteed at the code level (ARCHITECTURE.md §3.2).

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    // E0000-E0499: lexical
    TabCharacter,         // E0001
    UnterminatedString,   // E0002
    InvalidEscape,        // E0003
    InvalidNumberLiteral, // E0004
    UnknownToken,         // E0005

    // E0500-E0999: syntax
    IndentMismatch,         // E0501
    UnexpectedToken,        // E0502
    PipePlaceholderMissing, // E0503

    // E1000-E1999: type system
    DuplicateName,                   // E1001
    MissingParamAnnotation,          // E1002
    UninferableType,                 // E1003
    CollectionElementTypeMismatch,   // E1010
    DictKeyTypeNotAllowed,           // E1011
    SetElementTypeNotAllowed,        // E1012
    UnsupportedOperatorForTypeParam, // E1013
    BranchTypeMismatch,              // E1020
    NonExhaustiveMatch,              // E1021
    UnusedResult,                    // E1040
    IntFloatMixed,                   // E1050
    UnorderableType,                 // E1051
    QuestionOperatorMismatch,        // E1060
    ParallelQuestionOperator,        // E1061

    // E2000-E2999: effect
    ImpureCallInPureFunction, // E2001
    UndeclaredEffect,         // E2002
    InvalidEffectName,        // E2003

    // E3000-E3999: mutability
    ImmutableMutation, // E3001

    // E4000-E4999: lint
    UnusedVariable,   // E4001
    UnusedFunction,   // E4002
    Shadowing,        // E4003
    UnreachableCode,  // E4004
    NamingConvention, // E4005

    // E5000-E5999: module
    ModuleDirectiveMalformed, // E5001
    ModuleTopLevelStatement,  // E5002

    // E6000-E6999: runtime abnormal termination
    IndexOutOfRange,         // E6001
    DivisionByZero,          // E6002
    IntegerOverflow,         // E6003
    AssertFailed,            // E6004
    TopLevelErrPropagation,  // E6005
    TopLevelNonePropagation, // E6006
    UnwrapFailed,            // E6007
    StackOverflow,           // E6008

    // E9000-E9999: pre-CLI-startup
    FileNotFound,     // E9001
    InvalidExtension, // E9002
    FileReadFailure,  // E9003
}

impl ErrorCode {
    #[must_use]
    pub const fn numeric(self) -> u32 {
        use ErrorCode::{
            AssertFailed, BranchTypeMismatch, CollectionElementTypeMismatch, DictKeyTypeNotAllowed,
            DivisionByZero, DuplicateName, FileNotFound, FileReadFailure, ImmutableMutation,
            ImpureCallInPureFunction, IndentMismatch, IndexOutOfRange, IntFloatMixed,
            IntegerOverflow, InvalidEffectName, InvalidEscape, InvalidExtension,
            InvalidNumberLiteral, MissingParamAnnotation, ModuleDirectiveMalformed,
            ModuleTopLevelStatement, NamingConvention, NonExhaustiveMatch,
            ParallelQuestionOperator, PipePlaceholderMissing, QuestionOperatorMismatch,
            SetElementTypeNotAllowed, Shadowing, StackOverflow, TabCharacter,
            TopLevelErrPropagation, TopLevelNonePropagation, UndeclaredEffect, UnexpectedToken,
            UninferableType, UnknownToken, UnorderableType, UnreachableCode,
            UnsupportedOperatorForTypeParam, UnterminatedString, UnusedFunction, UnusedResult,
            UnusedVariable, UnwrapFailed,
        };
        match self {
            TabCharacter => 1,
            UnterminatedString => 2,
            InvalidEscape => 3,
            InvalidNumberLiteral => 4,
            UnknownToken => 5,
            IndentMismatch => 501,
            UnexpectedToken => 502,
            PipePlaceholderMissing => 503,
            DuplicateName => 1001,
            MissingParamAnnotation => 1002,
            UninferableType => 1003,
            CollectionElementTypeMismatch => 1010,
            DictKeyTypeNotAllowed => 1011,
            SetElementTypeNotAllowed => 1012,
            UnsupportedOperatorForTypeParam => 1013,
            BranchTypeMismatch => 1020,
            NonExhaustiveMatch => 1021,
            UnusedResult => 1040,
            IntFloatMixed => 1050,
            UnorderableType => 1051,
            QuestionOperatorMismatch => 1060,
            ParallelQuestionOperator => 1061,
            ImpureCallInPureFunction => 2001,
            UndeclaredEffect => 2002,
            InvalidEffectName => 2003,
            ImmutableMutation => 3001,
            UnusedVariable => 4001,
            UnusedFunction => 4002,
            Shadowing => 4003,
            UnreachableCode => 4004,
            NamingConvention => 4005,
            ModuleDirectiveMalformed => 5001,
            ModuleTopLevelStatement => 5002,
            IndexOutOfRange => 6001,
            DivisionByZero => 6002,
            IntegerOverflow => 6003,
            AssertFailed => 6004,
            TopLevelErrPropagation => 6005,
            TopLevelNonePropagation => 6006,
            UnwrapFailed => 6007,
            StackOverflow => 6008,
            FileNotFound => 9001,
            InvalidExtension => 9002,
            FileReadFailure => 9003,
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "E{:04}", self.numeric())
    }
}

#[cfg(test)]
mod tests {
    use super::ErrorCode;

    #[test]
    fn display_formats_as_four_digit_code() {
        assert_eq!(ErrorCode::TabCharacter.to_string(), "E0001");
        assert_eq!(ErrorCode::DuplicateName.to_string(), "E1001");
        assert_eq!(ErrorCode::StackOverflow.to_string(), "E6008");
        assert_eq!(ErrorCode::InvalidExtension.to_string(), "E9002");
    }
}
