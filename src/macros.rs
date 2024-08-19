macro_rules! is {
    ($ty:ty, $($other:ty),+ $(,)?) => {
        crate::macros::is!({ core::any::TypeId::of::<$ty>() }, $($other),+)
    };
    ({$ty:expr}, $($other:ty),+ $(,)?) => {
        false $(|| $ty == core::any::TypeId::of::<$other>())+
    };
}
pub(crate) use is;

macro_rules! is_not {
    ($($tt:tt)*) => { !crate::macros::is!($($tt)*) };
}
pub(crate) use is_not;
