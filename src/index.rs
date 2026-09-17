//! Dense indices into the arrays a tier keeps its program in.

macro_rules! index {

    ($($name:ident),* $(,)?) => {$(
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub u32);

        impl $name {
            /// # Panics
            ///
            /// If the program outgrew the four billion of these it may have.
            #[must_use]
            pub fn at(index: usize) -> Self {
                Self(u32::try_from(index).expect("a program within 4G of these"))
            }

            #[must_use]
            pub fn index(self) -> usize {
                usize::try_from(self.0).expect("an index that fits a pointer")
            }
        }
    )*};
}

pub(crate) use index;
